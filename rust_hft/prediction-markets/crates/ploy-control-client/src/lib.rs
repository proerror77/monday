use ploy_operator_contracts::{
    ActiveAlert, AuditLogEntry, ControlPlaneErrorResponse, DeploymentApplyRequest,
    DeploymentControlRequest, DeploymentState, DeploymentSummary, DesiredState, OperatorEvent,
    OrderControlResponse, OrderReplaceRequest, PaperIntentRequest, PaperIntentResponse,
    PlatformMetrics, SystemStatus, TradingStateSnapshot,
};
use serde::de::DeserializeOwned;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct ControlPlaneClient {
    pub control_plane_addr: String,
    pub admin_token: Option<String>,
    pub operator_token: Option<String>,
    pub sidecar_token: Option<String>,
    pub runtime_root: PathBuf,
    connection: Arc<Mutex<Option<TcpStream>>>,
}

impl ControlPlaneClient {
    pub fn from_runtime_root(runtime_root: impl Into<PathBuf>) -> Self {
        Self {
            control_plane_addr: "127.0.0.1:8081".to_string(),
            admin_token: std::env::var("PLOY_ADMIN_TOKEN")
                .ok()
                .or_else(|| std::env::var("PLOY_API_ADMIN_TOKEN").ok())
                .or_else(|| std::env::var("PLOY_API_KEY").ok())
                .filter(|token| !token.trim().is_empty()),
            operator_token: std::env::var("PLOY_OPERATOR_TOKEN")
                .ok()
                .or_else(|| std::env::var("PLOY_API_OPERATOR_TOKEN").ok())
                .filter(|token| !token.trim().is_empty()),
            sidecar_token: std::env::var("PLOY_SIDECAR_AUTH_TOKEN")
                .ok()
                .filter(|token| !token.trim().is_empty()),
            runtime_root: runtime_root.into(),
            connection: Arc::new(Mutex::new(None)),
        }
    }

    pub fn system_status(&self) -> String {
        match self.read_status_snapshot() {
            Ok(status) => format!(
                "status={} uptime={}s version={}",
                status.status, status.uptime_seconds, status.version
            ),
            Err(err) => format!("status=unavailable error={err}"),
        }
    }

    pub fn list_deployments(&self) -> Vec<DeploymentSummary> {
        self.read_deployment_snapshots().unwrap_or_default()
    }

    pub fn trading_state(&self) -> Result<Vec<TradingStateSnapshot>, String> {
        self.read_trading_state_snapshot()
    }

    /// Read the trading state from the live control plane without falling back
    /// to a potentially stale on-disk snapshot.
    pub fn live_trading_state(&self) -> Result<Vec<TradingStateSnapshot>, String> {
        self.read_trading_state_over_http()
    }

    pub fn system_snapshot(&self) -> Result<SystemStatus, String> {
        self.read_status_snapshot()
    }

    /// Read system status from the live control plane without snapshot
    /// fallback. Long-running automation should use this when stale context
    /// would be unsafe.
    pub fn live_system_snapshot(&self) -> Result<SystemStatus, String> {
        self.read_status_over_http()
    }

    pub fn audit_logs(&self) -> Result<Vec<AuditLogEntry>, String> {
        self.get_json("/api/audit/logs")
    }

    pub fn system_metrics(&self) -> Result<PlatformMetrics, String> {
        self.get_json("/api/system/metrics")
    }

    pub fn system_alerts(&self) -> Result<Vec<ActiveAlert>, String> {
        self.get_json("/api/system/alerts")
    }

    pub fn deployment_summaries(&self) -> Result<Vec<DeploymentSummary>, String> {
        self.read_deployment_snapshots()
    }

    /// Read deployments from the live control plane without snapshot fallback.
    pub fn live_deployment_summaries(&self) -> Result<Vec<DeploymentSummary>, String> {
        self.read_deployments_over_http()
    }

    pub fn inspect_deployment(&self, deployment_id: &str) -> Result<DeploymentSummary, String> {
        match self.read_deployment_over_http(deployment_id) {
            Ok(deployment) => Ok(deployment),
            Err(err) if should_fallback_to_snapshot(&err) => self
                .deployment_summaries()?
                .into_iter()
                .find(|deployment| deployment.deployment_id == deployment_id)
                .ok_or_else(|| format!("deployment `{deployment_id}` was not found")),
            Err(err) => Err(err),
        }
    }

    pub fn recent_events(&self, limit: usize) -> Result<Vec<OperatorEvent>, String> {
        let mut stream = TcpStream::connect(&self.control_plane_addr)
            .map_err(|err| format!("connect {}: {err}", self.control_plane_addr))?;
        let request = format!(
            "GET /api/events/stream HTTP/1.1\r\nHost: {}\r\n{}Connection: close\r\n\r\n",
            self.control_plane_addr,
            self.authorization_headers()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|err| format!("write request: {err}"))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .map_err(|err| format!("set read timeout: {err}"))?;
        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|err| format!("read HTTP status: {err}"))?;
        if !status_line.contains("200") {
            return Err(format!("unexpected HTTP status: {}", status_line.trim()));
        }

        loop {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .map_err(|err| format!("read headers: {err}"))?;
            if bytes == 0 || line == "\r\n" {
                break;
            }
        }

        let mut events = Vec::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end();
                    if let Some(payload) = trimmed.strip_prefix("data: ") {
                        let event: OperatorEvent = serde_json::from_str(payload)
                            .map_err(|err| format!("parse event payload: {err}"))?;
                        events.push(event);
                        if events.len() >= limit {
                            break;
                        }
                    }
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(err) => return Err(format!("read event stream: {err}")),
            }
        }

        Ok(events)
    }

    pub fn inspect_trading_state(
        &self,
        deployment_id: &str,
    ) -> Result<TradingStateSnapshot, String> {
        self.trading_state()?
            .into_iter()
            .find(|state| state.deployment_id == deployment_id)
            .ok_or_else(|| format!("trading state for `{deployment_id}` was not found"))
    }

    pub fn apply_deployment(
        &self,
        request: &DeploymentApplyRequest,
    ) -> Result<DeploymentSummary, String> {
        self.send_json(
            "PUT",
            &format!("/api/deployments/{}", request.deployment_id),
            request,
        )
    }

    pub fn set_desired_state(
        &self,
        deployment_id: &str,
        desired_state: DesiredState,
    ) -> Result<DeploymentSummary, String> {
        self.send_json(
            "POST",
            &format!("/api/deployments/{deployment_id}/control"),
            &DeploymentControlRequest {
                desired_state: Some(desired_state),
                deployment_state: None,
            },
        )
    }

    pub fn set_deployment_state(
        &self,
        deployment_id: &str,
        deployment_state: DeploymentState,
    ) -> Result<DeploymentSummary, String> {
        self.send_json(
            "POST",
            &format!("/api/deployments/{deployment_id}/control"),
            &DeploymentControlRequest {
                desired_state: None,
                deployment_state: Some(deployment_state),
            },
        )
    }

    pub fn cancel_order(
        &self,
        deployment_id: &str,
        order_id: &str,
    ) -> Result<OrderControlResponse, String> {
        self.send_empty(
            "POST",
            &format!("/api/deployments/{deployment_id}/orders/{order_id}/cancel"),
        )
    }

    pub fn replace_order(
        &self,
        deployment_id: &str,
        order_id: &str,
        request: &OrderReplaceRequest,
    ) -> Result<OrderControlResponse, String> {
        self.send_json(
            "POST",
            &format!("/api/deployments/{deployment_id}/orders/{order_id}/replace"),
            request,
        )
    }

    pub fn submit_intent(
        &self,
        deployment_id: &str,
        request: &PaperIntentRequest,
    ) -> Result<PaperIntentResponse, String> {
        self.send_json(
            "POST",
            &format!("/api/deployments/{deployment_id}/intents"),
            request,
        )
    }

    pub fn submit_worker_intent(
        &self,
        deployment_id: &str,
        request: &PaperIntentRequest,
    ) -> Result<PaperIntentResponse, String> {
        let worker_token = std::env::var("PLOY_WORKER_TOKEN")
            .ok()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| {
                "PLOY_WORKER_TOKEN is required for worker intent submission".to_string()
            })?;
        self.submit_worker_intent_with_token(deployment_id, request, &worker_token)
    }

    fn submit_worker_intent_with_token(
        &self,
        deployment_id: &str,
        request: &PaperIntentRequest,
        worker_token: &str,
    ) -> Result<PaperIntentResponse, String> {
        self.send_json_with_headers(
            "POST",
            &format!("/api/deployments/{deployment_id}/intents"),
            request,
            &format!("x-ploy-worker-token: {worker_token}\r\n"),
        )
    }

    pub fn worker_scoped(&self) -> Self {
        Self {
            control_plane_addr: self.control_plane_addr.clone(),
            admin_token: None,
            operator_token: None,
            sidecar_token: None,
            runtime_root: self.runtime_root.clone(),
            connection: Arc::clone(&self.connection),
        }
    }

    fn read_status_snapshot(&self) -> Result<SystemStatus, String> {
        match self.read_status_over_http() {
            Ok(status) => return Ok(status),
            Err(err) if !should_fallback_to_snapshot(&err) => return Err(err),
            Err(_) => {}
        }
        let body = fs::read_to_string(self.runtime_root.join("system-status.json"))
            .map_err(|err| format!("read status snapshot: {err}"))?;
        serde_json::from_str(&body).map_err(|err| format!("parse status snapshot: {err}"))
    }

    fn read_deployment_snapshots(&self) -> Result<Vec<DeploymentSummary>, String> {
        match self.read_deployments_over_http() {
            Ok(deployments) => return Ok(deployments),
            Err(err) if !should_fallback_to_snapshot(&err) => return Err(err),
            Err(_) => {}
        }
        let body = fs::read_to_string(self.runtime_root.join("deployments.json"))
            .map_err(|err| format!("read deployment snapshot: {err}"))?;
        serde_json::from_str(&body).map_err(|err| format!("parse deployment snapshot: {err}"))
    }

    fn read_trading_state_snapshot(&self) -> Result<Vec<TradingStateSnapshot>, String> {
        match self.read_trading_state_over_http() {
            Ok(snapshot) => return Ok(snapshot),
            Err(err) if !should_fallback_to_snapshot(&err) => return Err(err),
            Err(_) => {}
        }
        let body = fs::read_to_string(self.runtime_root.join("trading-state.json"))
            .map_err(|err| format!("read trading state snapshot: {err}"))?;
        serde_json::from_str(&body).map_err(|err| format!("parse trading state snapshot: {err}"))
    }

    fn read_status_over_http(&self) -> Result<SystemStatus, String> {
        self.get_json("/api/system/status")
    }

    fn read_deployments_over_http(&self) -> Result<Vec<DeploymentSummary>, String> {
        self.get_json("/api/deployments")
    }

    fn read_deployment_over_http(&self, deployment_id: &str) -> Result<DeploymentSummary, String> {
        self.get_json(&format!("/api/deployments/{deployment_id}"))
    }

    fn read_trading_state_over_http(&self) -> Result<Vec<TradingStateSnapshot>, String> {
        self.get_json("/api/trading/state")
    }

    fn get_json<T>(&self, path: &str) -> Result<T, String>
    where
        T: DeserializeOwned,
    {
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\n{}Connection: keep-alive\r\n\r\n",
            self.control_plane_addr,
            self.authorization_headers()
        );
        let (status_line, body) = self.send_request(&request)?;
        if !status_line.contains("200") {
            return Err(decode_http_error(&status_line, &body));
        }
        serde_json::from_str(&body).map_err(|err| format!("parse HTTP body: {err}"))
    }

    fn send_json<B, T>(&self, method: &str, path: &str, body: &B) -> Result<T, String>
    where
        B: serde::Serialize,
        T: DeserializeOwned,
    {
        self.send_json_with_headers(method, path, body, &self.authorization_headers())
    }

    fn send_json_with_headers<B, T>(
        &self,
        method: &str,
        path: &str,
        body: &B,
        authorization_headers: &str,
    ) -> Result<T, String>
    where
        B: serde::Serialize,
        T: DeserializeOwned,
    {
        let body = serde_json::to_string(body).map_err(|err| format!("serialize body: {err}"))?;
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\n{}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
            self.control_plane_addr,
            authorization_headers,
            body.len(),
            body
        );
        let (status_line, body) = self.send_request(&request)?;
        if !status_line.contains("200") {
            return Err(decode_http_error(&status_line, &body));
        }
        serde_json::from_str(&body).map_err(|err| format!("parse HTTP body: {err}"))
    }

    fn send_empty<T>(&self, method: &str, path: &str) -> Result<T, String>
    where
        T: DeserializeOwned,
    {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\n{}Connection: keep-alive\r\n\r\n",
            self.control_plane_addr,
            self.authorization_headers()
        );
        let (status_line, body) = self.send_request(&request)?;
        if !status_line.contains("200") {
            return Err(decode_http_error(&status_line, &body));
        }
        serde_json::from_str(&body).map_err(|err| format!("parse HTTP body: {err}"))
    }

    fn send_request(&self, request: &str) -> Result<(String, String), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "control-plane connection lock poisoned".to_string())?;
        if connection.as_mut().is_some_and(connection_closed) {
            *connection = None;
        }
        if connection.is_none() {
            let stream = TcpStream::connect(&self.control_plane_addr)
                .map_err(|err| format!("connect {}: {err}", self.control_plane_addr))?;
            let _ = stream.set_nodelay(true);
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(5)));
            *connection = Some(stream);
        }
        let stream = connection.as_mut().expect("connection initialized");
        if let Err(error) = stream.write_all(request.as_bytes()) {
            *connection = None;
            return Err(format!("write request: {error}"));
        }
        match read_http_response(stream) {
            Ok((status_line, body, close)) => {
                if close {
                    *connection = None;
                }
                Ok((status_line, body))
            }
            Err(error) => {
                *connection = None;
                Err(error)
            }
        }
    }

    fn authorization_headers(&self) -> String {
        if let Some(token) = &self.admin_token {
            format!("Authorization: Bearer {token}\r\nx-ploy-admin-token: {token}\r\n")
        } else if let Some(token) = &self.operator_token {
            format!("x-ploy-operator-token: {token}\r\n")
        } else if let Some(token) = &self.sidecar_token {
            format!("x-ploy-sidecar-token: {token}\r\n")
        } else {
            String::new()
        }
    }
}

fn connection_closed(stream: &mut TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut byte = [0_u8; 1];
    let closed = match stream.peek(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(_) => true,
    };
    let _ = stream.set_nonblocking(false);
    closed
}

fn read_http_response(stream: &mut TcpStream) -> Result<(String, String, bool), String> {
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|err| format!("read HTTP status: {err}"))?;
    if status_line.is_empty() {
        return Err("missing HTTP status line".to_string());
    }

    let mut content_length = None;
    let mut close = false;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|err| format!("read HTTP headers: {err}"))?;
        if line.is_empty() {
            return Err("missing HTTP body".to_string());
        }
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|err| format!("invalid HTTP Content-Length: {err}"))?,
                );
            } else if name.eq_ignore_ascii_case("connection") {
                close = value.trim().eq_ignore_ascii_case("close");
            }
        }
    }
    let content_length = content_length.ok_or_else(|| "missing HTTP Content-Length".to_string())?;
    let mut body = vec![0_u8; content_length];
    reader
        .read_exact(&mut body)
        .map_err(|err| format!("read HTTP body: {err}"))?;
    let body = String::from_utf8(body).map_err(|err| format!("decode HTTP body: {err}"))?;
    Ok((status_line, body, close))
}

fn decode_http_error(status_line: &str, body: &str) -> String {
    if let Ok(error) = serde_json::from_str::<ControlPlaneErrorResponse>(body) {
        match error.message {
            Some(message) => format!("HTTP {} {}: {}", status_line.trim(), error.error, message),
            None => format!("HTTP {} {}", status_line.trim(), error.error),
        }
    } else {
        format!("unexpected HTTP status: {}", status_line.trim())
    }
}

fn should_fallback_to_snapshot(error: &str) -> bool {
    error.starts_with("connect ")
        || error.starts_with("write request:")
        || error.starts_with("read response:")
        || error.starts_with("missing HTTP ")
}

impl Default for ControlPlaneClient {
    fn default() -> Self {
        Self::from_runtime_root(Path::new("run/platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::ControlPlaneClient;
    use ploy_operator_contracts::{
        DeploymentApplyRequest, DeploymentSnapshotEvent, DeploymentState, DeploymentSummary,
        DesiredState, IntentPurpose, ObservedState, OperatorEvent, OrderControlResponse,
        OrderReplaceRequest, PaperIntentRequest, SystemSnapshotEvent, SystemStatus,
        TradingStateSnapshot,
    };
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployctl-{label}-{unique}"))
    }

    #[test]
    fn worker_live_submit_uses_control_plane_client() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 4096];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request.starts_with("POST /api/deployments/example.live/intents"));
            assert!(request.contains("x-ploy-worker-token: worker-token"));
            assert!(!request.contains("Authorization: Bearer"));
            assert!(request.contains("\"idempotency_key\":\"intent-1\""));
            let body = serde_json::json!({
                "deployment_id": "example.live",
                "intent_id": "daemon-intent-1",
                "order_id": "order-daemon-intent-1",
                "state": "acknowledged",
                "venue_order_id": "venue-1",
                "rejection_reason": null,
                "last_error": null
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::default();
        client.control_plane_addr = addr.to_string();
        client.admin_token = Some("admin-token".to_string());
        client.operator_token = Some("operator-token".to_string());
        client.sidecar_token = Some("sidecar-token".to_string());
        let worker_client = client.worker_scoped();
        assert_eq!(worker_client.control_plane_addr, client.control_plane_addr);
        assert_eq!(worker_client.runtime_root, client.runtime_root);
        assert!(worker_client.admin_token.is_none());
        assert!(worker_client.operator_token.is_none());
        assert!(worker_client.sidecar_token.is_none());

        let response = worker_client
            .submit_worker_intent_with_token(
                "example.live",
                &PaperIntentRequest {
                    idempotency_key: Some("intent-1".to_string()),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: "buy".to_string(),
                    quantity: rust_decimal::Decimal::ONE,
                    limit_price: Some(rust_decimal::Decimal::new(45, 2)),
                    purpose: IntentPurpose::Entry,
                },
                "worker-token",
            )
            .expect("submit intent");

        assert_eq!(response.state, "acknowledged");
        server.join().expect("server");
    }

    #[test]
    fn worker_scope_clears_all_elevated_tokens() {
        let mut client = ControlPlaneClient::default();
        client.admin_token = Some("admin-token".to_string());
        client.operator_token = Some("operator-token".to_string());
        client.sidecar_token = Some("sidecar-token".to_string());

        let worker_client = client.worker_scoped();
        assert!(worker_client.admin_token.is_none());
        assert!(worker_client.operator_token.is_none());
        assert!(worker_client.sidecar_token.is_none());
    }

    #[test]
    fn client_reads_system_and_deployment_snapshots() {
        let runtime_root = temp_dir("runtime");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-status.json"),
            serde_json::to_string(&SystemStatus {
                status: "running".to_string(),
                uptime_seconds: 42,
                version: "0.1.0".to_string(),
                strategy: "platform".to_string(),
                last_trade_time: None,
                websocket_connected: false,
                database_connected: false,
                error_count_1h: 0,
                live_reconcile_failures: 0,
                next_live_reconcile_at: None,
                last_live_reconcile_error: None,
                active_alert_count: 0,
                stale_source_count: 0,
                last_live_reconcile_success_at: None,
            })
            .expect("status json"),
        )
        .expect("write status");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::to_string(&vec![DeploymentSummary {
                deployment_id: "example.paper".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(rust_decimal::Decimal::new(500, 2)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
                observed_state: ObservedState::Running,
            }])
            .expect("deployments json"),
        )
        .expect("write deployments");

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = "127.0.0.1:9".to_string();
        assert!(client.system_status().contains("running"));

        let deployments = client.list_deployments();
        assert_eq!(deployments.len(), 1);
        assert_eq!(
            client
                .inspect_deployment("example.paper")
                .expect("deployment"),
            deployments[0]
        );
        fs::write(
            runtime_root.join("trading-state.json"),
            serde_json::to_string(&vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                ..TradingStateSnapshot::default()
            }])
            .expect("trading json"),
        )
        .expect("write trading state");
        assert_eq!(
            client
                .inspect_trading_state("example.paper")
                .expect("trading state")
                .risk
                .open_positions,
            0,
        );
    }

    #[test]
    fn client_prefers_http_control_plane_when_available() {
        let runtime_root = temp_dir("http-runtime");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0_u8; 1024];
                let bytes = stream.read(&mut request).expect("read request");
                let request = String::from_utf8_lossy(&request[..bytes]);
                let body = if request.starts_with("GET /api/system/status") {
                    serde_json::json!({
                        "status": "running-via-http",
                        "uptime_seconds": 99,
                        "version": "0.1.0",
                        "strategy": "platform",
                        "last_trade_time": null,
                        "websocket_connected": false,
                        "database_connected": false,
                        "error_count_1h": 0
                    })
                    .to_string()
                } else if request.starts_with("GET /api/deployments/http.paper") {
                    serde_json::json!({
                        "deployment_id": "http.paper",
                        "account_id": "acct-http",
                        "max_gross_exposure": "5.00",
                        "deployment_state": "enabled",
                        "desired_state": "running",
                        "observed_state": "running"
                    })
                    .to_string()
                } else {
                    serde_json::json!([
                        {
                            "deployment_id": "http.paper",
                            "account_id": "acct-http",
                            "max_gross_exposure": "5.00",
                            "deployment_state": "enabled",
                            "desired_state": "running",
                            "observed_state": "running"
                        }
                    ])
                    .to_string()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write response");
            }
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        assert!(client.system_status().contains("running-via-http"));
        assert_eq!(
            client
                .inspect_deployment("http.paper")
                .expect("deployment over http")
                .deployment_id,
            "http.paper"
        );
    }

    #[test]
    fn client_applies_and_controls_deployment_over_http() {
        let runtime_root = temp_dir("http-mutate");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0_u8; 2048];
                let bytes = stream.read(&mut request).expect("read request");
                let request = String::from_utf8_lossy(&request[..bytes]);
                let body = if request.starts_with("PUT /api/deployments/example.paper") {
                    serde_json::json!({
                        "deployment_id": "example.paper",
                        "account_id": "acct-paper",
                        "max_gross_exposure": "5.00",
                        "deployment_state": "enabled",
                        "desired_state": "running",
                        "observed_state": "starting"
                    })
                    .to_string()
                } else {
                    serde_json::json!({
                        "deployment_id": "example.paper",
                        "account_id": "acct-paper",
                        "max_gross_exposure": "5.00",
                        "deployment_state": "enabled",
                        "desired_state": "paused",
                        "observed_state": "paused"
                    })
                    .to_string()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write response");
            }
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let applied = client
            .apply_deployment(&DeploymentApplyRequest {
                deployment_id: "example.paper".to_string(),
                bundle_id: "example".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(rust_decimal::Decimal::new(500, 2)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
            })
            .expect("apply");
        assert_eq!(applied.deployment_id, "example.paper");

        let paused = client
            .set_desired_state("example.paper", DesiredState::Paused)
            .expect("pause");
        assert_eq!(paused.desired_state, DesiredState::Paused);
    }

    #[test]
    fn client_reads_recent_events_from_sse() {
        let runtime_root = temp_dir("http-events");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let event1 =
                serde_json::to_string(&OperatorEvent::SystemSnapshot(SystemSnapshotEvent {
                    system: SystemStatus {
                        status: "running".to_string(),
                        uptime_seconds: 7,
                        version: "0.1.0".to_string(),
                        strategy: "platform".to_string(),
                        last_trade_time: None,
                        websocket_connected: false,
                        database_connected: false,
                        error_count_1h: 0,
                        live_reconcile_failures: 0,
                        next_live_reconcile_at: None,
                        last_live_reconcile_error: None,
                        active_alert_count: 0,
                        stale_source_count: 0,
                        last_live_reconcile_success_at: None,
                    },
                }))
                .expect("event1");
            let event2 = serde_json::to_string(&OperatorEvent::DeploymentSnapshot(
                DeploymentSnapshotEvent {
                    deployments: vec![DeploymentSummary {
                        deployment_id: "example.paper".to_string(),
                        runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                        account_id: "acct-paper".to_string(),
                        max_gross_exposure: Some(rust_decimal::Decimal::new(500, 2)),
                        deployment_state: DeploymentState::Enabled,
                        desired_state: DesiredState::Running,
                        observed_state: ObservedState::Running,
                    }],
                },
            ))
            .expect("event2");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {event1}\n\ndata: {event2}\n\n"
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let events = client.recent_events(2).expect("recent events");
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], OperatorEvent::SystemSnapshot(_)));
        assert!(matches!(events[1], OperatorEvent::DeploymentSnapshot(_)));
    }

    #[test]
    fn client_preserves_http_error_body_details() {
        let runtime_root = temp_dir("http-error");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::json!({
                "error": "deployment_not_found",
                "message": "deployment `missing.paper` was not found",
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let error = client
            .inspect_deployment("missing.paper")
            .expect_err("http error");
        assert!(error.contains("404"));
        assert!(error.contains("deployment_not_found"));
        assert!(error.contains("missing.paper"));
    }

    #[test]
    fn client_does_not_fallback_to_stale_status_snapshot_on_structured_http_error() {
        let runtime_root = temp_dir("status-http-error");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-status.json"),
            serde_json::json!({
                "status": "stale-snapshot",
                "uptime_seconds": 1,
                "version": "0.1.0",
                "strategy": "platform",
                "last_trade_time": null,
                "websocket_connected": false,
                "database_connected": false,
                "error_count_1h": 0
            })
            .to_string(),
        )
        .expect("write stale snapshot");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::json!({
                "error": "daemon_lock_poisoned",
                "message": "daemon state is unavailable",
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let error = client.system_snapshot().expect_err("structured http error");
        assert!(error.contains("daemon_lock_poisoned"));
        assert!(!error.contains("stale-snapshot"));
    }

    #[test]
    fn client_cancels_order_over_http() {
        let runtime_root = temp_dir("http-cancel");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 1024];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request.starts_with("POST /api/deployments/example.live/orders/order-1/cancel"));

            let body = serde_json::to_string(&OrderControlResponse {
                deployment_id: "example.live".to_string(),
                order_id: "order-1".to_string(),
                state: "canceled".to_string(),
                venue_order_id: Some("venue-1".to_string()),
                venue_order_history: vec!["venue-0".to_string()],
                revision: 1,
                requested_qty: Default::default(),
                limit_price: None,
                rejection_reason: None,
                last_error: None,
                filled_qty: Default::default(),
            })
            .expect("serialize response");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let client = ControlPlaneClient {
            control_plane_addr: addr.to_string(),
            admin_token: None,
            operator_token: None,
            sidecar_token: None,
            runtime_root,
            connection: Default::default(),
        };

        let response = client
            .cancel_order("example.live", "order-1")
            .expect("cancel response");
        assert_eq!(response.state, "canceled");
        assert_eq!(response.venue_order_id.as_deref(), Some("venue-1"));
    }

    #[test]
    fn client_replaces_order_over_http() {
        let runtime_root = temp_dir("http-replace");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(
                request.starts_with("POST /api/deployments/example.live/orders/order-1/replace")
            );
            assert!(
                request.contains("\"quantity\":\"2.5\"")
                    || request.contains("\"quantity\":\"2.50\"")
            );
            assert!(request.contains("\"limit_price\":\"0.57\""));

            let body = serde_json::to_string(&OrderControlResponse {
                deployment_id: "example.live".to_string(),
                order_id: "order-1".to_string(),
                state: "acknowledged".to_string(),
                venue_order_id: Some("venue-2".to_string()),
                venue_order_history: vec!["venue-1".to_string()],
                revision: 1,
                requested_qty: rust_decimal::Decimal::new(250, 2),
                limit_price: Some(rust_decimal::Decimal::new(57, 2)),
                rejection_reason: None,
                last_error: None,
                filled_qty: Default::default(),
            })
            .expect("serialize response");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let client = ControlPlaneClient {
            control_plane_addr: addr.to_string(),
            admin_token: None,
            operator_token: None,
            sidecar_token: None,
            runtime_root,
            connection: Default::default(),
        };

        let response = client
            .replace_order(
                "example.live",
                "order-1",
                &OrderReplaceRequest {
                    quantity: rust_decimal::Decimal::new(250, 2),
                    limit_price: Some(rust_decimal::Decimal::new(57, 2)),
                },
            )
            .expect("replace response");
        assert_eq!(response.state, "acknowledged");
        assert_eq!(response.revision, 1);
        assert_eq!(response.venue_order_id.as_deref(), Some("venue-2"));
        assert_eq!(response.venue_order_history, vec!["venue-1".to_string()]);
    }

    #[test]
    fn client_sends_admin_token_headers_when_configured() {
        let runtime_root = temp_dir("http-auth");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(request.contains("Authorization: Bearer secret-token"));
            assert!(request.contains("x-ploy-admin-token: secret-token"));

            let body = serde_json::json!({
                "status": "running",
                "uptime_seconds": 1,
                "version": "0.1.0",
                "strategy": "platform",
                "last_trade_time": null,
                "websocket_connected": false,
                "database_connected": false,
                "error_count_1h": 0
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let client = ControlPlaneClient {
            control_plane_addr: addr.to_string(),
            admin_token: Some("secret-token".to_string()),
            operator_token: None,
            sidecar_token: None,
            runtime_root,
            connection: Default::default(),
        };

        let status = client.system_snapshot().expect("status");
        assert_eq!(status.status, "running");
    }

    #[test]
    fn client_sends_operator_header_when_configured() {
        let runtime_root = temp_dir("http-operator-auth");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let bytes = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..bytes]);
            assert!(!request.contains("Authorization: Bearer"));
            assert!(request.contains("x-ploy-operator-token: operator-token"));

            let body = serde_json::json!({
                "deployment_id": "example.paper",
                "account_id": "acct-paper",
                "max_gross_exposure": null,
                "deployment_state": "enabled",
                "desired_state": "paused",
                "observed_state": "paused"
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let client = ControlPlaneClient {
            control_plane_addr: addr.to_string(),
            admin_token: None,
            operator_token: Some("operator-token".to_string()),
            sidecar_token: None,
            runtime_root,
            connection: Default::default(),
        };

        let deployment = client
            .set_desired_state("example.paper", DesiredState::Paused)
            .expect("deployment");
        assert_eq!(deployment.desired_state, DesiredState::Paused);
    }

    #[test]
    fn client_reuses_keep_alive_connection() {
        let runtime_root = temp_dir("http-keep-alive");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept once");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .expect("read timeout");
            for _ in 0..2 {
                let mut request = Vec::new();
                let mut byte = [0_u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).expect("read request");
                    request.push(byte[0]);
                }
                assert!(String::from_utf8_lossy(&request).contains("Connection: keep-alive"));
                let body = serde_json::json!({
                    "status":"running",
                    "uptime_seconds":1,
                    "version":"0.1.0",
                    "strategy":"platform",
                    "last_trade_time":null,
                    "websocket_connected":false,
                    "database_connected":false,
                    "error_count_1h":0,
                    "live_reconcile_failures":0,
                    "next_live_reconcile_at":null,
                    "last_live_reconcile_error":null
                })
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: keep-alive\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write response");
            }
        });

        let mut client = ControlPlaneClient::from_runtime_root(runtime_root);
        client.control_plane_addr = addr.to_string();
        assert_eq!(client.system_snapshot().expect("first").status, "running");
        assert_eq!(client.system_snapshot().expect("second").status, "running");
        server.join().expect("server");
    }

    #[test]
    fn client_reconnects_after_idle_keep_alive_socket_closes() {
        let runtime_root = temp_dir("http-stale-keep-alive");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0_u8; 1024];
                let bytes_read = stream.read(&mut request).expect("read request");
                assert!(bytes_read > 0, "request must not be empty");
                let body = serde_json::json!({
                    "status":"running",
                    "uptime_seconds":1,
                    "version":"0.1.0",
                    "strategy":"platform",
                    "last_trade_time":null,
                    "websocket_connected":false,
                    "database_connected":false,
                    "error_count_1h":0,
                    "live_reconcile_failures":0,
                    "next_live_reconcile_at":null,
                    "last_live_reconcile_error":null
                })
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: keep-alive\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write response");
            }
        });

        let mut client = ControlPlaneClient::from_runtime_root(runtime_root);
        client.control_plane_addr = addr.to_string();
        assert_eq!(client.system_snapshot().expect("first").status, "running");
        thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(client.system_snapshot().expect("second").status, "running");
        server.join().expect("server");
    }
}
