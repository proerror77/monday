use crate::events::EventBroker;
use crate::reports::{
    generate_dry_run_summary_json, generate_market_data_health_json, generate_strategy_report_html,
};
use crate::runtime::{next_paper_intent_id, PloyDaemon, PreparedIntentSubmission};
use chrono::Utc;
use hmac::{Hmac, Mac};
use ploy_operator_contracts::{
    compute_oversight_report, validate_agent_run_create_request, AgentRunCreateRequest,
    AgentRunCreateResponse, AgentRunRecord, AlertSnapshotEvent, AuditLogEntry,
    ControlPlaneErrorResponse, DeploymentApplyRequest, DeploymentControlRequest,
    DeploymentDiagnosticsReport, DeploymentSnapshotEvent, DiagnosticsEvidence, DiagnosticsFinding,
    IntentPurpose, MetricsSnapshotEvent, OperatorEvent, OrderReplaceRequest,
    OversightSnapshotEvent, PaperIntentRequest, PlatformDiagnosticsReport, ProposalCreateRequest,
    ProposalDecisionRequest, ProposalSnapshotEvent, StatusUpdate, SystemSnapshotEvent,
    SystemStatus, TradingSnapshotEvent,
};
use ploy_platform_runtime::runtime_support::IntentAdmissionSource;
use ploy_trading::{TradeSide, TradingIntent};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use sha2::Sha256;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[cfg(test)]
use crate::config::PlatformConfig;

#[derive(Debug)]
pub struct AppState {
    pub daemon: Arc<Mutex<PloyDaemon>>,
    pub events: Arc<EventBroker>,
}

const ADMIN_SESSION_COOKIE_NAME: &str = "ploy_admin_session";
const AUDIT_LOG_TAIL_LIMIT: usize = 200;
const MAX_HTTP_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_JSONL_TAIL_BYTES: usize = 1024 * 1024;
type HmacSha256 = Hmac<Sha256>;
static REQUEST_RATE_LIMITER: OnceLock<Mutex<RateLimiter>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthLevel {
    None,
    Worker,
    Sidecar,
    Operator,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequiredAccess {
    Public,
    ReadOnly,
    Intent,
    Operator,
    Admin,
}

#[derive(Debug, Default)]
struct RateLimiter {
    requests: BTreeMap<String, VecDeque<Instant>>,
    last_pruned: Option<Instant>,
}

impl RateLimiter {
    fn allow(&mut self, key: &str, limit_per_minute: u32) -> bool {
        let now = Instant::now();
        if self
            .last_pruned
            .is_none_or(|last_pruned| now.duration_since(last_pruned) >= Duration::from_secs(1))
        {
            let cutoff = now - Duration::from_secs(60);
            self.requests.retain(|_, bucket| {
                while matches!(bucket.front(), Some(timestamp) if *timestamp < cutoff) {
                    bucket.pop_front();
                }
                !bucket.is_empty()
            });
            self.last_pruned = Some(now);
        }
        if limit_per_minute == 0 {
            return true;
        }
        // ponytail: bounded in-memory limiter; move to a shared limiter if 10k active clients/minute is legitimate.
        if !self.requests.contains_key(key) && self.requests.len() >= 10_000 {
            return false;
        }

        let bucket = self.requests.entry(key.to_string()).or_default();
        if bucket.len() >= limit_per_minute as usize {
            return false;
        }
        bucket.push_back(now);
        true
    }
}

pub fn render_status(status: &SystemStatus) -> String {
    format!(
        "status={} uptime={}s version={}",
        status.status, status.uptime_seconds, status.version
    )
}

fn json_error(status: u16, error: &str, message: impl Into<Option<String>>) -> (u16, String) {
    (
        status,
        serde_json::to_string(&ControlPlaneErrorResponse {
            error: error.to_string(),
            message: message.into(),
        })
        .unwrap_or_else(|_| {
            "{\"error\":\"serialization_failed\",\"message\":\"failed to serialize control-plane error\"}".to_string()
        }),
    )
}

fn status_text(status_code: u16) -> &'static str {
    match status_code {
        200 => "OK",
        202 => "Accepted",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

fn submit_intent_error_response(err: io::Error, deployment_id: &str) -> (u16, String) {
    match err.kind() {
        io::ErrorKind::NotFound => json_error(
            404,
            "deployment_not_found",
            Some(format!("deployment `{deployment_id}` was not found")),
        ),
        io::ErrorKind::InvalidInput => json_error(400, "invalid_request", Some(err.to_string())),
        io::ErrorKind::InvalidData => {
            json_error(503, "live_execution_misconfigured", Some(err.to_string()))
        }
        io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::TimedOut
        | io::ErrorKind::NotConnected
        | io::ErrorKind::BrokenPipe => {
            json_error(503, "live_execution_unavailable", Some(err.to_string()))
        }
        _ => json_error(500, "submit_failed", Some(err.to_string())),
    }
}

#[cfg(test)]
pub fn route_request(path: &str, runtime_root: &Path) -> (u16, String) {
    let target = match path {
        "/health" | "/api/system/status" => runtime_root.join("system-status.json"),
        "/api/deployments" => runtime_root.join("deployments.json"),
        "/api/trading/state" => runtime_root.join("trading-state.json"),
        _ => {
            return json_error(404, "not_found", None);
        }
    };

    match fs::read_to_string(target) {
        Ok(body) => (200, body),
        Err(_) => json_error(503, "snapshot_unavailable", None),
    }
}

fn host_root_from_runtime_root(runtime_root: &Path) -> PathBuf {
    runtime_root
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn query_param(raw_path: &str, key: &str) -> Option<String> {
    let (_, query) = raw_path.split_once('?')?;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=')?;
        if name == key && !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn build_strategy_report_html(state: &Arc<AppState>, since: Option<&str>) -> (u16, String) {
    let host_root = match state.daemon.lock() {
        Ok(daemon) => host_root_from_runtime_root(&daemon.config.runtime_root),
        Err(_) => return html_error(503, "daemon lock poisoned"),
    };

    match generate_strategy_report_html(&host_root, since) {
        Ok(body) => (200, body),
        Err(err) => html_error(500, &err),
    }
}

fn build_market_data_health_json(state: &Arc<AppState>) -> (u16, String) {
    match state.daemon.lock() {
        Ok(_) => {}
        Err(_) => return json_error(503, "daemon_lock_poisoned", None),
    }

    report_json_response(
        generate_market_data_health_json(),
        "market_data_health_unavailable",
    )
}

fn build_dry_run_summary_json(state: &Arc<AppState>) -> (u16, String) {
    let host_root = match state.daemon.lock() {
        Ok(daemon) => host_root_from_runtime_root(&daemon.config.runtime_root),
        Err(_) => return json_error(503, "daemon_lock_poisoned", None),
    };

    report_json_response(
        generate_dry_run_summary_json(&host_root),
        "dry_run_summary_unavailable",
    )
}

fn report_json_response(result: Result<String, String>, error: &str) -> (u16, String) {
    match result {
        Ok(body) => (200, body),
        Err(message) => json_error(500, error, Some(message)),
    }
}

fn html_error(status_code: u16, message: &str) -> (u16, String) {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    (
        status_code,
        format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Strategy Report Error</title></head><body><h1>Strategy Report Error</h1><p>{escaped}</p></body></html>"
        ),
    )
}

#[cfg(test)]
pub fn handle_api_request(
    method: &str,
    path: &str,
    body: Option<&str>,
    config: &PlatformConfig,
) -> (u16, String) {
    match (method, path) {
        ("GET", "/health")
        | ("GET", "/api/system/status")
        | ("GET", "/api/deployments")
        | ("GET", "/api/trading/state") => route_request(path, &config.runtime_root),
        ("GET", _) if path.starts_with("/api/deployments/") && !path.ends_with("/control") => {
            let deployment_id = path.trim_start_matches("/api/deployments/");
            match PloyDaemon::boot(config)
                .ok()
                .and_then(|daemon| daemon.inspect_deployment(deployment_id))
            {
                Some(record) => (
                    200,
                    serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                ),
                None => (404, "{\"error\":\"deployment_not_found\"}".to_string()),
            }
        }
        ("PUT", _) if path.starts_with("/api/deployments/") => {
            let deployment_id = path.trim_start_matches("/api/deployments/");
            let Some(body) = body else {
                return (400, "{\"error\":\"missing_body\"}".to_string());
            };
            let request: DeploymentApplyRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(_) => return (400, "{\"error\":\"invalid_json\"}".to_string()),
            };
            if request.deployment_id != deployment_id {
                return (400, "{\"error\":\"deployment_id_mismatch\"}".to_string());
            }
            match PloyDaemon::boot(config).and_then(|mut daemon| daemon.apply_deployment(request)) {
                Ok(record) => (
                    200,
                    serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                ),
                Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                    (400, "{\"error\":\"apply_failed\"}".to_string())
                }
                Err(_) => (500, "{\"error\":\"apply_failed\"}".to_string()),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/control") => {
            let deployment_id = path
                .trim_start_matches("/api/deployments/")
                .trim_end_matches("/control")
                .trim_end_matches('/');
            let Some(body) = body else {
                return (400, "{\"error\":\"missing_body\"}".to_string());
            };
            let request: DeploymentControlRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(_) => return (400, "{\"error\":\"invalid_json\"}".to_string()),
            };
            match PloyDaemon::boot(config)
                .and_then(|mut daemon| daemon.control_deployment(deployment_id, request))
            {
                Ok(Some(record)) => (
                    200,
                    serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                ),
                Ok(None) => (404, "{\"error\":\"deployment_not_found\"}".to_string()),
                Err(err) if err.kind() == io::ErrorKind::InvalidInput => (
                    400,
                    format!("{{\"error\":\"invalid_request\",\"message\":\"{}\"}}", err),
                ),
                Err(_) => (500, "{\"error\":\"control_failed\"}".to_string()),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/intents") => {
            let deployment_id = path
                .trim_start_matches("/api/deployments/")
                .trim_end_matches("/intents")
                .trim_end_matches('/');
            let Some(body) = body else {
                return (400, "{\"error\":\"missing_body\"}".to_string());
            };
            let request: PaperIntentRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(_) => return (400, "{\"error\":\"invalid_json\"}".to_string()),
            };
            let side = match trade_side_from_wire(&request.side) {
                Ok(side) => side,
                Err(error) => return json_error(400, &error.error, error.message),
            };

            match PloyDaemon::boot(config) {
                Ok(mut daemon) => {
                    let response = daemon.submit_intent_idempotent(
                        TradingIntent {
                            intent_id: request
                                .idempotency_key
                                .as_deref()
                                .map(|key| format!("request-{key}"))
                                .unwrap_or_else(|| next_paper_intent_id(deployment_id)),
                            deployment_id: deployment_id.to_string(),
                            market_id: request.market_id,
                            token_id: request.token_id,
                            side,
                            quantity: request.quantity,
                            limit_price: request.limit_price,
                            purpose: intent_purpose_from_wire(request.purpose),
                            created_at: chrono::Utc::now(),
                        },
                        request.idempotency_key.as_deref(),
                    );
                    match response {
                        Ok(response) => (
                            200,
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => submit_intent_error_response(err, deployment_id),
                    }
                }
                Err(err) => submit_intent_error_response(err, deployment_id),
            }
        }
        _ => (404, "{\"error\":\"not_found\"}".to_string()),
    }
}

pub fn spawn_server(state: Arc<AppState>) -> io::Result<thread::JoinHandle<()>> {
    let listen_addr = state
        .daemon
        .lock()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "daemon lock poisoned"))?
        .config
        .listen_addr
        .clone();
    let listener = TcpListener::bind(&listen_addr)?;
    Ok(thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let state = state.clone();
            thread::spawn(move || {
                let _ = handle_connection(stream, &state);
            });
        }
    }))
}

fn handle_connection(mut stream: TcpStream, state: &Arc<AppState>) -> io::Result<()> {
    loop {
        let request = read_http_request(&mut stream)?;
        if request.is_empty() {
            return Ok(());
        }
        let keep_alive = request.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("connection")
                    && value.trim().eq_ignore_ascii_case("keep-alive")
            })
        });

        let mut request_line = request.lines().next().unwrap_or("").split_whitespace();
        let method = request_line.next().unwrap_or("GET");
        let raw_path = request_line.next().unwrap_or("/");
        let path = raw_path.split('?').next().unwrap_or(raw_path);
        let peer_addr = stream.peer_addr().ok().map(|addr| addr.to_string());
        let client_addr = Some(client_ip(peer_addr.as_deref(), &request));
        let (configured_token, operator_token, worker_token, sidecar_token, cookie_secret) =
            match configured_auth(state) {
                Ok(auth) => auth,
                Err(response) => return write_json_response(stream, response),
            };
        let auth_level = request_auth_level(
            &request,
            configured_token
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(|token| token.as_str()),
            operator_token
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(|token| token.as_str()),
            worker_token
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(|token| token.as_str()),
            sidecar_token
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(|token| token.as_str()),
            cookie_secret.expose_secret(),
        );
        let required_access = required_access(method, path);
        if let Some(response) =
            rate_limit_response(method, path, client_addr.as_deref(), auth_level, state)
        {
            audit_request(
                state,
                method,
                path,
                client_addr.as_deref(),
                auth_level,
                required_access,
                response.0,
                "rate_limited",
                response_message(&response.1),
            );
            return write_json_response(stream, response);
        }
        if method == "GET" && path == "/api/events/stream" {
            if !access_allowed(
                auth_level,
                required_access,
                configured_token.is_some(),
                operator_token.is_some(),
                worker_token.is_some(),
                sidecar_token.is_some(),
            ) {
                let response = auth_error_response(
                    required_access,
                    configured_token.is_some(),
                    operator_token.is_some(),
                    worker_token.is_some(),
                    sidecar_token.is_some(),
                );
                audit_request(
                    state,
                    method,
                    path,
                    client_addr.as_deref(),
                    auth_level,
                    required_access,
                    response.0,
                    "denied",
                    response_message(&response.1),
                );
                return write_json_response(stream, response);
            }
            audit_request(
                state,
                method,
                path,
                client_addr.as_deref(),
                auth_level,
                required_access,
                200,
                "allowed",
                None,
            );
            return handle_event_stream(stream, state);
        }
        if method == "GET" && path == "/reports/strategy" {
            if !access_allowed(
                auth_level,
                required_access,
                configured_token.is_some(),
                operator_token.is_some(),
                worker_token.is_some(),
                sidecar_token.is_some(),
            ) {
                let response = auth_error_response(
                    required_access,
                    configured_token.is_some(),
                    operator_token.is_some(),
                    worker_token.is_some(),
                    sidecar_token.is_some(),
                );
                audit_request(
                    state,
                    method,
                    path,
                    client_addr.as_deref(),
                    auth_level,
                    required_access,
                    response.0,
                    "denied",
                    response_message(&response.1),
                );
                return write_json_response(stream, response);
            }

            let response =
                build_strategy_report_html(state, query_param(raw_path, "since").as_deref());
            audit_request(
                state,
                method,
                path,
                client_addr.as_deref(),
                auth_level,
                required_access,
                response.0,
                if response.0 < 400 {
                    "allowed"
                } else {
                    "denied"
                },
                if response.0 < 400 {
                    None
                } else {
                    Some("strategy report generation failed".to_string())
                },
            );
            return write_html_response(stream, response);
        }
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .filter(|body| !body.is_empty());
        let response = handle_authenticated_runtime_request(
            method,
            path,
            body,
            auth_level,
            configured_token.is_some(),
            operator_token.is_some(),
            worker_token.is_some(),
            sidecar_token.is_some(),
            state,
        );
        let headers = response_headers(
            method,
            path,
            response.0,
            configured_token
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(|token| token.as_str()),
            cookie_secret.expose_secret(),
        );
        let status_code = response.0;
        let outcome = if status_code < 400 {
            "allowed"
        } else {
            "denied"
        };
        let message = response_message(&response.1);
        if method == "POST" && path.ends_with("/intents") {
            let result = if keep_alive {
                write_json_response_keep_alive_with_headers(stream.try_clone()?, response, &headers)
            } else {
                write_json_response_with_headers(stream.try_clone()?, response, &headers)
            };
            audit_request(
                state,
                method,
                path,
                client_addr.as_deref(),
                auth_level,
                required_access,
                status_code,
                outcome,
                message,
            );
            result?;
            if keep_alive {
                continue;
            }
            return Ok(());
        }
        audit_request(
            state,
            method,
            path,
            client_addr.as_deref(),
            auth_level,
            required_access,
            status_code,
            outcome,
            message,
        );
        if keep_alive {
            write_json_response_keep_alive_with_headers(stream.try_clone()?, response, &headers)?;
            continue;
        }
        return write_json_response_with_headers(stream, response, &headers);
    }
}

fn read_http_request(stream: &mut TcpStream) -> io::Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];

    loop {
        let bytes = stream.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..bytes]);
        if request.len() > MAX_HTTP_REQUEST_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP request exceeded maximum request size",
            ));
        }

        if let Some(header_end) = header_end_offset(&request) {
            let expected_body_bytes = content_length(&request[..header_end])?;
            if request.len() >= header_end + 4 + expected_body_bytes {
                break;
            }
            if expected_body_bytes == 0 {
                break;
            }
        }
    }

    Ok(String::from_utf8_lossy(&request).into_owned())
}

fn header_end_offset(request: &[u8]) -> Option<usize> {
    request.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(headers: &[u8]) -> io::Result<usize> {
    let headers = String::from_utf8_lossy(headers);
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            let length = value.trim().parse::<usize>().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid Content-Length header: {error}"),
                )
            })?;
            if length > MAX_HTTP_REQUEST_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP request body exceeded maximum request size",
                ));
            }
            return Ok(length);
        }
    }
    Ok(0)
}

fn write_json_response(stream: TcpStream, response: (u16, String)) -> io::Result<()> {
    write_json_response_with_headers(stream, response, &[])
}

fn write_html_response(stream: TcpStream, response: (u16, String)) -> io::Result<()> {
    write_response_with_headers(stream, response, "text/html; charset=utf-8", &[])
}

fn write_response_with_headers(
    stream: TcpStream,
    response: (u16, String),
    content_type: &str,
    headers: &[(String, String)],
) -> io::Result<()> {
    write_response_with_headers_and_connection(stream, response, content_type, headers, false)
}

fn write_response_with_headers_and_connection(
    mut stream: TcpStream,
    response: (u16, String),
    content_type: &str,
    headers: &[(String, String)],
    keep_alive: bool,
) -> io::Result<()> {
    let (status_code, body) = response;
    let extra_headers = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: {}\r\n{}\
         \r\n{}",
        status_code,
        status_text(status_code),
        body.len(),
        content_type,
        if keep_alive { "keep-alive" } else { "close" },
        extra_headers,
        body
    )
}

fn write_json_response_with_headers(
    stream: TcpStream,
    response: (u16, String),
    headers: &[(String, String)],
) -> io::Result<()> {
    write_response_with_headers(stream, response, "application/json", headers)
}

fn write_json_response_keep_alive_with_headers(
    stream: TcpStream,
    response: (u16, String),
    headers: &[(String, String)],
) -> io::Result<()> {
    write_response_with_headers_and_connection(stream, response, "application/json", headers, true)
}

fn configured_auth(
    state: &Arc<AppState>,
) -> Result<
    (
        Option<SecretString>,
        Option<SecretString>,
        Option<SecretString>,
        Option<SecretString>,
        SecretString,
    ),
    (u16, String),
> {
    state
        .daemon
        .lock()
        .map(|daemon| {
            (
                daemon.config.admin_token.clone(),
                daemon.config.operator_token.clone(),
                daemon.config.worker_token.clone(),
                daemon.config.sidecar_token.clone(),
                daemon.config.auth_cookie_secret.clone(),
            )
        })
        .map_err(|_| json_error(503, "daemon_lock_poisoned", None))
}

fn request_rate_limiter() -> &'static Mutex<RateLimiter> {
    REQUEST_RATE_LIMITER.get_or_init(|| Mutex::new(RateLimiter::default()))
}

fn request_rate_limit_per_minute(state: &Arc<AppState>) -> Result<u32, (u16, String)> {
    state
        .daemon
        .lock()
        .map(|daemon| daemon.config.request_rate_limit_per_minute)
        .map_err(|_| json_error(503, "daemon_lock_poisoned", None))
}

fn rate_limit_response(
    method: &str,
    path: &str,
    client_addr: Option<&str>,
    auth_level: AuthLevel,
    state: &Arc<AppState>,
) -> Option<(u16, String)> {
    if path == "/health" {
        return None;
    }
    let limit = match request_rate_limit_per_minute(state) {
        Ok(limit) => limit,
        Err(response) => return Some(response),
    };
    let key = rate_limit_key(client_addr, auth_level, method, path);
    let allowed = match request_rate_limiter().lock() {
        Ok(mut limiter) => limiter.allow(&key, limit),
        Err(_) => return Some(json_error(503, "rate_limiter_lock_poisoned", None)),
    };
    if allowed {
        None
    } else {
        Some(json_error(
            429,
            "rate_limited",
            Some(format!(
                "request rate limit exceeded for client `{}`",
                client_addr.unwrap_or("unknown")
            )),
        ))
    }
}

fn rate_limit_key(
    client_addr: Option<&str>,
    auth_level: AuthLevel,
    _method: &str,
    _path: &str,
) -> String {
    let client_ip = client_addr
        .and_then(|address| address.parse::<std::net::SocketAddr>().ok())
        .map(|address| address.ip().to_string())
        .unwrap_or_else(|| client_addr.unwrap_or("unknown").to_string());
    format!("{client_ip}|{}", auth_level_name(auth_level))
}

fn client_ip(peer_addr: Option<&str>, request: &str) -> String {
    let peer_ip = peer_addr
        .and_then(|address| address.parse::<std::net::SocketAddr>().ok())
        .map(|address| address.ip());
    if peer_ip.is_some_and(|ip| ip.is_loopback()) {
        if let Some(real_ip) = extract_header(request, "X-Real-IP")
            .and_then(|value| value.parse::<std::net::IpAddr>().ok())
        {
            return real_ip.to_string();
        }
    }
    peer_ip
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| peer_addr.unwrap_or("unknown").to_string())
}

fn required_access(method: &str, path: &str) -> RequiredAccess {
    match (method, path) {
        ("GET", "/health")
        | ("GET", "/auth/session")
        | ("POST", "/auth/login")
        | ("POST", "/auth/logout") => RequiredAccess::Public,
        ("GET", "/api/system/status")
        | ("GET", "/api/system/metrics")
        | ("GET", "/api/system/alerts")
        | ("GET", "/api/market-data/health")
        | ("GET", "/api/reports/dry-run")
        | ("GET", "/api/deployments")
        | ("GET", "/api/trading/state")
        | ("GET", "/reports/strategy")
        | ("GET", "/api/events/stream") => RequiredAccess::ReadOnly,
        ("GET", "/api/audit/logs") => RequiredAccess::Admin,
        ("GET", "/api/system/diagnose") => RequiredAccess::ReadOnly,
        ("GET", "/api/agent/runs") => RequiredAccess::ReadOnly,
        ("GET", "/api/agent/harness-memory") => RequiredAccess::ReadOnly,
        ("GET", _) if path.starts_with("/api/agent/runs/") => RequiredAccess::ReadOnly,
        ("POST", "/api/agent/runs") => RequiredAccess::Operator,
        ("GET", "/api/proposals") | ("POST", "/api/proposals") => RequiredAccess::ReadOnly,
        ("GET", _) if path.starts_with("/api/proposals/") => RequiredAccess::ReadOnly,
        ("POST", _) if path.starts_with("/api/proposals/") => RequiredAccess::Operator,
        ("GET", _) if path.starts_with("/api/deployments/") && !path.ends_with("/control") => {
            RequiredAccess::ReadOnly
        }
        ("GET", _) if path.starts_with("/api/trading/diagnose/") => RequiredAccess::ReadOnly,
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/intents") => {
            RequiredAccess::Intent
        }
        _ => RequiredAccess::Operator,
    }
}

fn auth_level_name(auth_level: AuthLevel) -> &'static str {
    match auth_level {
        AuthLevel::None => "none",
        AuthLevel::Worker => "worker",
        AuthLevel::Sidecar => "sidecar",
        AuthLevel::Operator => "operator",
        AuthLevel::Admin => "admin",
    }
}

fn required_access_name(required_access: RequiredAccess) -> &'static str {
    match required_access {
        RequiredAccess::Public => "public",
        RequiredAccess::ReadOnly => "read_only",
        RequiredAccess::Intent => "intent",
        RequiredAccess::Operator => "operator",
        RequiredAccess::Admin => "admin",
    }
}

fn response_message(body: &str) -> Option<String> {
    serde_json::from_str::<ControlPlaneErrorResponse>(body)
        .ok()
        .and_then(|error| error.message.or(Some(error.error)))
}

fn audit_log_path(state: &Arc<AppState>) -> Result<PathBuf, (u16, String)> {
    state
        .daemon
        .lock()
        .map(|daemon| daemon.config.audit_log_file.clone())
        .map_err(|_| json_error(503, "daemon_lock_poisoned", None))
}

fn audit_request(
    state: &Arc<AppState>,
    method: &str,
    path: &str,
    client_addr: Option<&str>,
    auth_level: AuthLevel,
    required_access: RequiredAccess,
    status_code: u16,
    outcome: &str,
    message: Option<String>,
) {
    if path == "/health" {
        return;
    }

    let Ok(audit_log_file) = audit_log_path(state) else {
        return;
    };
    let entry = AuditLogEntry {
        timestamp: Utc::now(),
        method: method.to_string(),
        path: path.to_string(),
        client_addr: client_addr.map(str::to_string),
        auth_level: auth_level_name(auth_level).to_string(),
        required_access: required_access_name(required_access).to_string(),
        status_code,
        outcome: outcome.to_string(),
        message,
    };
    let _ = append_audit_entry(&audit_log_file, &entry);
}

fn append_audit_entry(path: &PathBuf, entry: &AuditLogEntry) -> io::Result<()> {
    append_jsonl(path, entry)
}

fn append_jsonl<T>(path: &PathBuf, value: &T) -> io::Result<()>
where
    T: Serialize,
{
    let _lock = lock_jsonl(path)?;
    append_jsonl_unlocked(path, value)
}

fn append_jsonl_unlocked<T>(path: &Path, value: &T) -> io::Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    normalize_complete_jsonl_tail(path)?;
    let mut body =
        serde_json::to_vec(value).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    body.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&body)?;
    file.sync_data()
}

fn normalize_complete_jsonl_tail(path: &Path) -> io::Result<()> {
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut final_byte = [0_u8; 1];
    file.read_exact(&mut final_byte)?;
    if final_byte[0] == b'\n' {
        return Ok(());
    }

    let read_len = usize::try_from(len.min(MAX_JSONL_TAIL_BYTES as u64))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "JSONL tail is too large"))?;
    let start = len - read_len as u64;
    file.seek(SeekFrom::Start(start))?;
    let mut suffix = vec![0_u8; read_len];
    file.read_exact(&mut suffix)?;
    let tail_start = suffix
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|position| position + 1)
        .unwrap_or_default();
    if start > 0 && tail_start == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("JSONL tail exceeds {MAX_JSONL_TAIL_BYTES} bytes"),
        ));
    }
    serde_json::from_slice::<serde_json::Value>(&suffix[tail_start..]).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("refusing to append after truncated JSONL tail: {err}"),
        )
    })?;
    file.seek(SeekFrom::End(0))?;
    file.write_all(b"\n")?;
    file.sync_data()
}

fn lock_jsonl(path: &Path) -> io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = suffixed_path(path, ".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.lock()?;
    Ok(lock)
}

fn suffixed_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn read_recent_audit_entries(path: &PathBuf, limit: usize) -> io::Result<Vec<AuditLogEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(path)?;
    let mut entries = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<AuditLogEntry>(line).ok())
        .collect::<Vec<_>>();
    if entries.len() > limit {
        entries = entries.split_off(entries.len() - limit);
    }
    Ok(entries)
}

fn agent_runs_path(state: &Arc<AppState>) -> Result<PathBuf, (u16, String)> {
    state
        .daemon
        .lock()
        .map(|daemon| daemon.config.agent_runs_file.clone())
        .map_err(|_| json_error(503, "daemon_lock_poisoned", None))
}

fn read_agent_runs(path: &PathBuf) -> io::Result<Vec<AgentRunRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(path)?;
    let mut runs = Vec::new();
    for (index, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AgentRunRecord>(line) {
            Ok(run) => runs.push(run),
            Err(err) => eprintln!(
                "skipping invalid agent run record on line {} of {}: {err}",
                index + 1,
                path.display()
            ),
        }
    }
    runs.reverse();
    let mut seen = BTreeSet::new();
    runs.retain(|run| seen.insert(run.run_id.clone()));
    Ok(runs)
}

fn read_agent_run(path: &PathBuf, run_id: &str) -> io::Result<Option<AgentRunRecord>> {
    Ok(read_agent_runs(path)?
        .into_iter()
        .find(|run| run.run_id == run_id))
}

fn agent_run_requests_path(agent_runs_path: &Path) -> PathBuf {
    agent_runs_path
        .parent()
        .map(|parent| parent.join("agent-run-requests.jsonl"))
        .unwrap_or_else(|| PathBuf::from("agent-run-requests.jsonl"))
}

fn harness_context_path(agent_runs_path: &Path) -> PathBuf {
    agent_runs_path
        .parent()
        .map(|parent| parent.join("harness-context.md"))
        .unwrap_or_else(|| PathBuf::from("harness-context.md"))
}

fn harness_events_path(agent_runs_path: &Path) -> PathBuf {
    agent_runs_path
        .parent()
        .map(|parent| parent.join("harness-events.jsonl"))
        .unwrap_or_else(|| PathBuf::from("harness-events.jsonl"))
}

fn read_harness_events(path: &PathBuf, limit: usize) -> io::Result<Vec<serde_json::Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(path)?;
    let mut events = Vec::new();
    for (index, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(event) => events.push(event),
            Err(err) => eprintln!(
                "skipping invalid harness event on line {} of {}: {err}",
                index + 1,
                path.display()
            ),
        }
    }
    events.reverse();
    events.truncate(limit);
    Ok(events)
}

fn read_harness_memory(agent_runs_path: &PathBuf) -> io::Result<serde_json::Value> {
    let context_path = harness_context_path(agent_runs_path);
    let events_path = harness_events_path(agent_runs_path);
    let context = if context_path.exists() {
        fs::read_to_string(&context_path)?
    } else {
        String::new()
    };
    let events = read_harness_events(&events_path, 100)?;
    Ok(serde_json::json!({
        "context": context,
        "events": events,
        "event_count": events.len(),
        "updated_at": Utc::now(),
    }))
}

fn queue_agent_run_request(
    state: &Arc<AppState>,
    request: AgentRunCreateRequest,
) -> io::Result<AgentRunCreateResponse> {
    let run_id = format!("agent-{}", Uuid::new_v4());
    let created_at = Utc::now();
    let request_snapshot = request.clone();
    let agent_runs_file = agent_runs_path(state).map_err(|response| {
        io::Error::new(
            io::ErrorKind::Other,
            response_message(&response.1).unwrap_or(response.1),
        )
    })?;
    let request_file = agent_run_requests_path(&agent_runs_file);
    let platform_status = state
        .daemon
        .lock()
        .ok()
        .map(|daemon| daemon.control_plane.system.status().status.to_string());

    let queued = AgentRunRecord {
        run_id: run_id.clone(),
        cycle_kind: "agentic_strategy".to_string(),
        status: "requested".to_string(),
        started_at: created_at,
        finished_at: None,
        session_id: None,
        model: "sidecar".to_string(),
        platform_status,
        deployment_count: 0,
        oversight_signal_count: 0,
        oversight_playbook_count: 0,
        total_cost_usd: None,
        tool_calls: Vec::new(),
        research_reports: 0,
        oversight_alerts: 0,
        operator_recommendations: 0,
        failure_reason: None,
        runtime_context: Some(serde_json::json!({
            "request": {
                "objective": request_snapshot.objective,
                "strategy_profile": request_snapshot.strategy_profile,
                "autonomy_mode": request_snapshot.autonomy_mode,
                "target_evidence": request_snapshot.target_evidence,
                "symbols": request_snapshot.symbols,
                "max_turns": request_snapshot.max_turns,
                "budget_usd": request_snapshot.budget_usd,
            }
        })),
        output_summary: None,
        evaluation: None,
    };

    let _queue_lock = lock_jsonl(&request_file)?;
    append_jsonl_unlocked(
        &request_file,
        &serde_json::json!({
            "run_id": run_id.clone(),
            "created_at": created_at,
            "request": request,
        }),
    )?;
    append_jsonl(&agent_runs_file, &queued)?;

    Ok(AgentRunCreateResponse {
        run_id: queued.run_id,
        status: queued.status,
        message: "agent run request queued for sidecar processing".to_string(),
    })
}

fn build_platform_diagnostics_report(
    daemon: &PloyDaemon,
    state: &Arc<AppState>,
) -> io::Result<PlatformDiagnosticsReport> {
    let system = daemon.control_plane.system.status();
    let metrics = daemon.platform_metrics();
    let alerts = daemon.active_alerts();
    let deployments = daemon.control_plane.deployments.summaries();
    let trading = daemon.trading_state();
    let oversight = compute_oversight_report(&system, &deployments, &trading);
    let audit_entries = audit_log_path(state)
        .ok()
        .and_then(|path| read_recent_audit_entries(&path, 4).ok())
        .unwrap_or_default();
    let event_entries = recent_snapshot_evidence(daemon, 8, None);
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();

    for alert in alerts {
        let key = format!("{:?}:{}", alert.kind, alert.message);
        if seen.insert(key) {
            findings.push(DiagnosticsFinding {
                severity: format!("{:?}", alert.severity).to_lowercase(),
                kind: format!("{:?}", alert.kind).to_lowercase(),
                message: alert.message.clone(),
                first_observed_at: Some(alert.triggered_at.to_rfc3339()),
                likely_causes: likely_causes_from_kind(&format!("{:?}", alert.kind).to_lowercase()),
                operator_command: Some("ployctl system audit".to_string()),
                evidence: vec![DiagnosticsEvidence {
                    source: "system_alert".to_string(),
                    label: alert.source_id.clone(),
                    detail: format!("alert_id={}", alert.alert_id),
                    observed_at: Some(alert.triggered_at.to_rfc3339()),
                }],
            });
        }
    }

    for signal in oversight.signals {
        // Include deployment_id in the dedupe key so findings from different
        // deployments with the same kind/message are not collapsed into one.
        let key = format!(
            "{}:{}:{}",
            signal.kind,
            signal.message,
            signal.deployment_id.as_deref().unwrap_or("platform")
        );
        if seen.insert(key) {
            // Reuse the pre-built operator_command from recommended_actions so
            // the command shown here always matches what build_operator_command
            // produces (correct deployment id, config hints, etc.).
            let target = signal.deployment_id.as_deref().unwrap_or("platform");
            let operator_command = oversight
                .recommended_actions
                .iter()
                .find(|a| a.kind == signal.recommended_action && a.target == target)
                .map(|a| a.operator_command.clone())
                .or_else(|| {
                    // Fallback for signals that have no matching action entry.
                    Some(format!("ployctl system status"))
                });
            findings.push(DiagnosticsFinding {
                severity: signal.severity.clone(),
                kind: signal.kind.clone(),
                message: signal.message.clone(),
                first_observed_at: Some(oversight.timestamp.clone()),
                likely_causes: likely_causes_from_kind(&signal.kind),
                operator_command,
                evidence: signal
                    .evidence
                    .iter()
                    .map(|detail| DiagnosticsEvidence {
                        source: "oversight_signal".to_string(),
                        label: signal.recommended_action.clone(),
                        detail: detail.clone(),
                        observed_at: Some(oversight.timestamp.clone()),
                    })
                    .collect(),
            });
        }
    }

    if metrics.live_reconcile_failures > 0 {
        findings.push(DiagnosticsFinding {
            severity: "warning".to_string(),
            kind: "live_reconcile_failures".to_string(),
            message: format!(
                "live reconcile reported {} consecutive failures",
                metrics.live_reconcile_failures
            ),
            first_observed_at: metrics
                .last_live_reconcile_success_at
                .map(|value| value.to_rfc3339()),
            likely_causes: vec!["reconcile_loop_stalled".to_string()],
            operator_command: Some("ployctl system audit".to_string()),
            evidence: vec![DiagnosticsEvidence {
                source: "system_metrics".to_string(),
                label: "live_reconcile_failures".to_string(),
                detail: format!(
                    "live_reconcile_failures={}",
                    metrics.live_reconcile_failures
                ),
                observed_at: metrics
                    .last_live_reconcile_success_at
                    .map(|value| value.to_rfc3339()),
            }],
        });
    }

    let mut recent_evidence = audit_entries
        .iter()
        .map(audit_entry_to_evidence)
        .collect::<Vec<_>>();
    recent_evidence.extend(event_entries);
    recent_evidence.truncate(8);

    Ok(PlatformDiagnosticsReport {
        generated_at: Utc::now().to_rfc3339(),
        platform_status: system.status.clone(),
        first_diverged_metric: if system.active_alert_count > 0 {
            Some("active_alerts".to_string())
        } else if system.stale_source_count > 0 {
            Some("stale_sources".to_string())
        } else if system.error_count_1h > 0 {
            Some("error_count_1h".to_string())
        } else if metrics.live_reconcile_failures > 0 {
            Some("live_reconcile_failures".to_string())
        } else {
            findings.first().map(|finding| finding.kind.clone())
        },
        findings,
        recent_evidence,
    })
}

fn build_deployment_diagnostics_report(
    daemon: &PloyDaemon,
    state: &Arc<AppState>,
    deployment_id: &str,
) -> io::Result<DeploymentDiagnosticsReport> {
    let system = daemon.control_plane.system.status();
    let deployments = daemon.control_plane.deployments.summaries();
    let deployment = daemon.inspect_deployment(deployment_id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("deployment `{deployment_id}` was not found"),
        )
    })?;
    let trading = daemon.trading_state();
    let state_snapshot = trading
        .iter()
        .find(|item| item.deployment_id == deployment_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("trading state for `{deployment_id}` was not found"),
            )
        })?;
    let oversight = compute_oversight_report(&system, &deployments, &trading);
    let audit_entries = audit_log_path(state)
        .ok()
        .and_then(|path| read_recent_audit_entries(&path, 8).ok())
        .unwrap_or_default();
    let mut recent_evidence = vec![DiagnosticsEvidence {
        source: "current_snapshot".to_string(),
        label: "trading_state".to_string(),
        detail: format!(
            "pending_intents={} active_orders={} open_positions={} total_gross_exposure={} net_pnl={}",
            state_snapshot.risk.pending_intents,
            state_snapshot.risk.active_orders,
            state_snapshot.risk.open_positions,
            state_snapshot.risk.total_gross_exposure,
            state_snapshot.pnl.net_pnl,
        ),
        observed_at: Some(oversight.timestamp.clone()),
    }];
    recent_evidence.extend(
        audit_entries
            .iter()
            .filter(|entry| {
                entry.path.contains(deployment_id)
                    || entry
                        .message
                        .as_deref()
                        .map(|message| message.contains(deployment_id))
                        .unwrap_or(false)
            })
            .map(audit_entry_to_evidence),
    );
    recent_evidence.extend(recent_snapshot_evidence(daemon, 12, Some(deployment_id)));
    recent_evidence.truncate(8);

    let mut findings = Vec::new();
    for signal in oversight
        .signals
        .iter()
        .filter(|signal| signal.deployment_id.as_deref() == Some(deployment_id))
    {
        let action = oversight.recommended_actions.iter().find(|action| {
            action.target == deployment_id && action.kind == signal.recommended_action
        });
        findings.push(DiagnosticsFinding {
            severity: signal.severity.clone(),
            kind: signal.kind.clone(),
            message: signal.message.clone(),
            first_observed_at: Some(oversight.timestamp.clone()),
            likely_causes: likely_causes_from_kind(&signal.kind),
            operator_command: action.map(|item| item.operator_command.clone()),
            evidence: signal
                .evidence
                .iter()
                .map(|detail| DiagnosticsEvidence {
                    source: "oversight_signal".to_string(),
                    label: signal.recommended_action.clone(),
                    detail: detail.clone(),
                    observed_at: Some(oversight.timestamp.clone()),
                })
                .collect(),
        });
    }

    let desired_state = format!("{:?}", deployment.desired_state).to_lowercase();
    let observed_state = format!("{:?}", deployment.observed_state).to_lowercase();
    let primary_diagnosis = findings
        .first()
        .map(|finding| finding.kind.clone())
        .unwrap_or_else(|| {
            if desired_state != observed_state {
                "state_mismatch".to_string()
            } else {
                "stable".to_string()
            }
        });

    Ok(DeploymentDiagnosticsReport {
        generated_at: Utc::now().to_rfc3339(),
        deployment_id: deployment.deployment_id.clone(),
        bundle_id: deployment.bundle_id.clone(),
        runtime_mode: deployment.runtime_mode.clone(),
        account_id: deployment.account_id.clone(),
        desired_state: desired_state.clone(),
        observed_state: observed_state.clone(),
        max_gross_exposure: deployment.max_gross_exposure,
        primary_diagnosis,
        first_diverged_metric: if desired_state != observed_state {
            Some("state_mismatch".to_string())
        } else {
            findings.first().map(|finding| finding.kind.clone())
        },
        metrics: ploy_operator_contracts::DeploymentDiagnosticsMetrics {
            pending_intents: state_snapshot.risk.pending_intents,
            active_orders: state_snapshot.risk.active_orders,
            open_positions: state_snapshot.risk.open_positions,
            fills: state_snapshot.fills.len(),
            positions: state_snapshot.positions.len(),
            gross_exposure: state_snapshot.risk.gross_exposure,
            reserved_order_exposure: state_snapshot.risk.reserved_order_exposure,
            total_gross_exposure: state_snapshot.risk.total_gross_exposure,
            net_pnl: state_snapshot.pnl.net_pnl,
        },
        findings,
        recent_evidence,
    })
}

fn recent_snapshot_evidence(
    daemon: &PloyDaemon,
    limit: usize,
    deployment_id: Option<&str>,
) -> Vec<DiagnosticsEvidence> {
    snapshot_events(daemon)
        .into_iter()
        .filter_map(|event| event_to_evidence(&event, deployment_id))
        .take(limit)
        .collect()
}

fn event_to_evidence(
    event: &OperatorEvent,
    deployment_id: Option<&str>,
) -> Option<DiagnosticsEvidence> {
    match event {
        OperatorEvent::Log(log) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: format!("log:{}", log.component),
            detail: log.message.clone(),
            observed_at: Some(log.timestamp.to_rfc3339()),
        }),
        OperatorEvent::Status(status) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "status".to_string(),
            detail: format!("platform status={}", status.status),
            observed_at: None,
        }),
        OperatorEvent::SystemSnapshot(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "system_snapshot".to_string(),
            detail: format!(
                "status={} errors_1h={} active_alerts={} stale_sources={}",
                event.system.status,
                event.system.error_count_1h,
                event.system.active_alert_count,
                event.system.stale_source_count,
            ),
            observed_at: event.system.last_trade_time.map(|value| value.to_rfc3339()),
        }),
        OperatorEvent::MetricsSnapshot(event) => Some(DiagnosticsEvidence {
            source: "event_stream".to_string(),
            label: "metrics_snapshot".to_string(),
            detail: format!(
                "deployments_total={} degraded={} active_alerts={} stale_sources={} live_reconcile_failures={}",
                event.metrics.total_deployments,
                event.metrics.degraded_deployments,
                event.metrics.active_alerts,
                event.metrics.stale_sources,
                event.metrics.live_reconcile_failures,
            ),
            observed_at: event
                .metrics
                .last_live_reconcile_success_at
                .map(|value| value.to_rfc3339()),
        }),
        OperatorEvent::AlertSnapshot(event) => {
            event.alerts.first().map(|alert| DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "alert_snapshot".to_string(),
                detail: format!(
                    "{} {} {}",
                    format!("{:?}", alert.severity).to_lowercase(),
                    format!("{:?}", alert.kind).to_lowercase(),
                    alert.message
                ),
                observed_at: Some(alert.triggered_at.to_rfc3339()),
            })
        }
        OperatorEvent::OversightSnapshot(event) => {
            if let Some(target) = deployment_id {
                if !event
                    .oversight
                    .signals
                    .iter()
                    .any(|signal| signal.deployment_id.as_deref() == Some(target))
                    && !event
                        .oversight
                        .recommended_actions
                        .iter()
                        .any(|action| action.target == target)
                {
                    return None;
                }
            }
            Some(DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "oversight_snapshot".to_string(),
                detail: format!(
                    "platform_status={} signal_count={} deployments_reviewed={}",
                    event.oversight.platform_status,
                    event.oversight.signal_count,
                    event.oversight.deployments_reviewed,
                ),
                observed_at: Some(event.oversight.timestamp.clone()),
            })
        }
        OperatorEvent::ProposalSnapshot(event) => {
            let count = deployment_id
                .map(|target| {
                    event
                        .proposals
                        .iter()
                        .filter(|proposal| proposal.target_deployment_id == target)
                        .count()
                })
                .unwrap_or(event.proposals.len());
            if count == 0 {
                return None;
            }
            Some(DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "proposal_snapshot".to_string(),
                detail: format!("proposals={count}"),
                observed_at: None,
            })
        }
        OperatorEvent::DeploymentSnapshot(event) => {
            if let Some(target) = deployment_id {
                if !event
                    .deployments
                    .iter()
                    .any(|deployment| deployment.deployment_id == target)
                {
                    return None;
                }
            }
            Some(DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "deployment_snapshot".to_string(),
                detail: format!("deployments={}", event.deployments.len()),
                observed_at: None,
            })
        }
        OperatorEvent::TradingSnapshot(event) => {
            if let Some(target) = deployment_id {
                if !event
                    .trading
                    .iter()
                    .any(|snapshot| snapshot.deployment_id == target)
                {
                    return None;
                }
            }
            Some(DiagnosticsEvidence {
                source: "event_stream".to_string(),
                label: "trading_snapshot".to_string(),
                detail: format!("deployments={}", event.trading.len()),
                observed_at: None,
            })
        }
        _ => None,
    }
}

fn audit_entry_to_evidence(entry: &AuditLogEntry) -> DiagnosticsEvidence {
    DiagnosticsEvidence {
        source: "audit_log".to_string(),
        label: format!("{} {}", entry.method, entry.path),
        detail: format!(
            "status={} auth={} required={} outcome={} client={} {}",
            entry.status_code,
            entry.auth_level,
            entry.required_access,
            entry.outcome,
            entry.client_addr.as_deref().unwrap_or("-"),
            entry.message.as_deref().unwrap_or("-"),
        ),
        observed_at: Some(entry.timestamp.to_rfc3339()),
    }
}

fn likely_causes_from_kind(kind: &str) -> Vec<String> {
    match kind {
        "system_errors" => vec!["control_plane_instability".to_string()],
        "state_mismatch" => vec!["worker_lifecycle_divergence".to_string()],
        "order_buildup" => vec!["fill_quality_deterioration".to_string()],
        "position_buildup" => vec!["exit_path_stalled".to_string()],
        "exposure_watch" => vec!["risk_budget_pressure".to_string()],
        "pnl_regression" => vec!["strategy_regime_shift".to_string()],
        "source_stale" => vec!["data_feed_staleness".to_string()],
        _ => Vec::new(),
    }
}

fn extract_header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let (header_name, value) = line.split_once(':')?;
            if header_name.eq_ignore_ascii_case(name) {
                Some(value.trim())
            } else {
                None
            }
        })
}

fn extract_cookie<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    let header = extract_header(request, "Cookie")?;
    header.split(';').find_map(|entry| {
        let (cookie_name, value) = entry.trim().split_once('=')?;
        if cookie_name == name {
            Some(value.trim())
        } else {
            None
        }
    })
}

fn request_auth_level(
    request: &str,
    admin_token: Option<&str>,
    operator_token: Option<&str>,
    worker_token: Option<&str>,
    sidecar_token: Option<&str>,
    cookie_secret: &str,
) -> AuthLevel {
    if let Some(expected_token) = admin_token {
        if let Some(header) = extract_header(request, "Authorization") {
            if let Some(token) = header.strip_prefix("Bearer ") {
                if token.trim() == expected_token {
                    return AuthLevel::Admin;
                }
            }
        }

        if extract_header(request, "x-ploy-admin-token")
            .map(|token| token == expected_token)
            .unwrap_or(false)
        {
            return AuthLevel::Admin;
        }

        if extract_cookie(request, ADMIN_SESSION_COOKIE_NAME)
            .map(|token| cookie_matches(token, expected_token, cookie_secret))
            .unwrap_or(false)
        {
            return AuthLevel::Admin;
        }
    }

    if let Some(expected_token) = operator_token {
        if extract_header(request, "x-ploy-operator-token")
            .map(|token| token == expected_token)
            .unwrap_or(false)
        {
            return AuthLevel::Operator;
        }
    }

    if let Some(expected_token) = worker_token {
        if extract_header(request, "x-ploy-worker-token")
            .map(|token| token == expected_token)
            .unwrap_or(false)
        {
            return AuthLevel::Worker;
        }
    }

    if let Some(expected_token) = sidecar_token {
        if extract_header(request, "x-ploy-sidecar-token")
            .map(|token| token == expected_token)
            .unwrap_or(false)
        {
            return AuthLevel::Sidecar;
        }
    }

    AuthLevel::None
}

fn access_allowed(
    auth_level: AuthLevel,
    required_access: RequiredAccess,
    _admin_configured: bool,
    _operator_configured: bool,
    _worker_configured: bool,
    _sidecar_configured: bool,
) -> bool {
    match required_access {
        RequiredAccess::Public => true,
        RequiredAccess::ReadOnly => matches!(
            auth_level,
            AuthLevel::Admin | AuthLevel::Operator | AuthLevel::Sidecar
        ),
        RequiredAccess::Intent => matches!(
            auth_level,
            AuthLevel::Admin | AuthLevel::Operator | AuthLevel::Worker
        ),
        RequiredAccess::Operator => matches!(auth_level, AuthLevel::Admin | AuthLevel::Operator),
        RequiredAccess::Admin => auth_level == AuthLevel::Admin,
    }
}

fn auth_error_response(
    required_access: RequiredAccess,
    admin_configured: bool,
    operator_configured: bool,
    worker_configured: bool,
    sidecar_configured: bool,
) -> (u16, String) {
    let message = match required_access {
        RequiredAccess::Public => return (200, "{}".to_string()),
        RequiredAccess::ReadOnly
            if sidecar_configured && !operator_configured && !admin_configured =>
        {
            "control-plane sidecar, operator, or admin token is required".to_string()
        }
        RequiredAccess::ReadOnly => {
            "control-plane admin, operator, or sidecar token is required".to_string()
        }
        RequiredAccess::Intent if admin_configured || operator_configured || worker_configured => {
            "control-plane worker, operator, or admin token is required".to_string()
        }
        RequiredAccess::Intent => {
            "control-plane worker, operator, or admin authentication is not configured".to_string()
        }
        RequiredAccess::Operator if admin_configured || operator_configured => {
            "control-plane operator or admin token is required".to_string()
        }
        RequiredAccess::Operator => "control-plane operator token is required".to_string(),
        RequiredAccess::Admin => "control-plane admin token is required".to_string(),
    };
    json_error(401, "unauthorized", Some(message))
}

fn response_headers(
    method: &str,
    path: &str,
    status_code: u16,
    configured_token: Option<&str>,
    cookie_secret: &str,
) -> Vec<(String, String)> {
    if status_code != 200 {
        return Vec::new();
    }

    match (method, path, configured_token) {
        ("POST", "/auth/login", Some(token)) => {
            vec![(
                "Set-Cookie".to_string(),
                admin_session_cookie(token, cookie_secret),
            )]
        }
        ("POST", "/auth/logout", _) => vec![(
            "Set-Cookie".to_string(),
            clear_admin_session_cookie().to_string(),
        )],
        _ => Vec::new(),
    }
}

fn admin_session_cookie(token: &str, cookie_secret: &str) -> String {
    let signature = sign_admin_session(token, cookie_secret);
    format!("{ADMIN_SESSION_COOKIE_NAME}=v1.{signature}; HttpOnly; Path=/; SameSite=Strict")
}

fn clear_admin_session_cookie() -> &'static str {
    "ploy_admin_session=; HttpOnly; Path=/; SameSite=Strict; Max-Age=0"
}

fn sign_admin_session(token: &str, cookie_secret: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(cookie_secret.as_bytes()).expect("hmac accepts arbitrary keys");
    mac.update(token.as_bytes());
    bytes_to_hex(&mac.finalize().into_bytes())
}

fn cookie_matches(cookie_value: &str, expected_token: &str, cookie_secret: &str) -> bool {
    cookie_value
        .strip_prefix("v1.")
        .map(|signature| signature == sign_admin_session(expected_token, cookie_secret))
        .unwrap_or(false)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn intent_admission_source(auth_level: AuthLevel) -> Option<IntentAdmissionSource> {
    match auth_level {
        AuthLevel::Worker => Some(IntentAdmissionSource::Worker),
        AuthLevel::Admin | AuthLevel::Operator => {
            Some(IntentAdmissionSource::AuthenticatedOperator)
        }
        AuthLevel::None | AuthLevel::Sidecar => None,
    }
}

fn handle_authenticated_runtime_request(
    method: &str,
    path: &str,
    body: Option<&str>,
    auth_level: AuthLevel,
    admin_configured: bool,
    operator_configured: bool,
    worker_configured: bool,
    sidecar_configured: bool,
    state: &Arc<AppState>,
) -> (u16, String) {
    let required_access = required_access(method, path);
    match (method, path) {
        ("GET", "/auth/session") => (
            200,
            serde_json::json!({
                "authenticated": auth_level == AuthLevel::Admin,
                "auth_required": true,
                "operator_authenticated": auth_level == AuthLevel::Operator,
                "worker_authenticated": auth_level == AuthLevel::Worker,
                "sidecar_authenticated": auth_level == AuthLevel::Sidecar,
            })
            .to_string(),
        ),
        ("POST", "/auth/login") => {
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let provided = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|value| {
                    value
                        .get("admin_token")
                        .and_then(|token| token.as_str())
                        .map(str::to_string)
                });
            if !admin_configured {
                return json_error(503, "admin_auth_not_configured", None);
            }

            match state.daemon.lock() {
                Ok(daemon) => match daemon
                    .config
                    .admin_token
                    .as_ref()
                    .map(ExposeSecret::expose_secret)
                {
                    Some(expected) if provided.as_deref() == Some(expected) => {
                        (200, serde_json::json!({ "success": true }).to_string())
                    }
                    Some(_) => json_error(
                        401,
                        "invalid_credentials",
                        Some(
                            "admin token did not match configured control-plane token".to_string(),
                        ),
                    ),
                    None => json_error(503, "admin_auth_not_configured", None),
                },
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", "/auth/logout") => (200, serde_json::json!({ "success": true }).to_string()),
        _ if !access_allowed(
            auth_level,
            required_access,
            admin_configured,
            operator_configured,
            worker_configured,
            sidecar_configured,
        ) =>
        {
            auth_error_response(
                required_access,
                admin_configured,
                operator_configured,
                worker_configured,
                sidecar_configured,
            )
        }
        _ => handle_runtime_request_from(
            method,
            path,
            body,
            intent_admission_source(auth_level),
            state,
        ),
    }
}

fn handle_runtime_request(
    method: &str,
    path: &str,
    body: Option<&str>,
    state: &Arc<AppState>,
) -> (u16, String) {
    handle_runtime_request_from(
        method,
        path,
        body,
        Some(IntentAdmissionSource::AuthenticatedOperator),
        state,
    )
}

fn handle_runtime_request_from(
    method: &str,
    path: &str,
    body: Option<&str>,
    intent_source: Option<IntentAdmissionSource>,
    state: &Arc<AppState>,
) -> (u16, String) {
    match (method, path) {
        ("GET", "/health") | ("GET", "/api/system/status") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.control_plane.system.status())
                    .unwrap_or_else(|_| "{}".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/system/metrics") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.platform_metrics())
                    .unwrap_or_else(|_| "{}".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/system/alerts") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.active_alerts()).unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/market-data/health") => build_market_data_health_json(state),
        ("GET", "/api/reports/dry-run") => build_dry_run_summary_json(state),
        ("GET", "/api/deployments") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.control_plane.deployments.summaries())
                    .unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/trading/state") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.trading_state()).unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/audit/logs") => match audit_log_path(state)
            .map_err(|response| response)
            .and_then(|path| {
                read_recent_audit_entries(&path, AUDIT_LOG_TAIL_LIMIT)
                    .map_err(|err| json_error(500, "audit_log_unavailable", Some(err.to_string())))
            }) {
            Ok(entries) => (
                200,
                serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(response) => response,
        },
        ("GET", "/api/system/diagnose") => match state.daemon.lock() {
            Ok(daemon) => match build_platform_diagnostics_report(&daemon, state) {
                Ok(report) => (
                    200,
                    serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string()),
                ),
                Err(err) => json_error(500, "diagnostics_unavailable", Some(err.to_string())),
            },
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", "/api/agent/runs") => match agent_runs_path(state)
            .map_err(|response| response)
            .and_then(|path| {
                read_agent_runs(&path)
                    .map_err(|err| json_error(500, "agent_runs_unavailable", Some(err.to_string())))
            }) {
            Ok(runs) => (
                200,
                serde_json::to_string(&runs).unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(response) => response,
        },
        ("GET", "/api/agent/harness-memory") => match agent_runs_path(state)
            .map_err(|response| response)
            .and_then(|path| {
                read_harness_memory(&path).map_err(|err| {
                    json_error(500, "harness_memory_unavailable", Some(err.to_string()))
                })
            }) {
            Ok(memory) => (
                200,
                serde_json::to_string(&memory).unwrap_or_else(|_| "{}".to_string()),
            ),
            Err(response) => response,
        },
        ("POST", "/api/agent/runs") => {
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: AgentRunCreateRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };
            if let Err(reason) = validate_agent_run_create_request(&request) {
                return json_error(400, "agent_run_limits_exceeded", Some(reason));
            }
            match queue_agent_run_request(state, request) {
                Ok(response) => (
                    202,
                    serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                ),
                Err(err) => json_error(500, "agent_run_queue_failed", Some(err.to_string())),
            }
        }
        ("GET", _) if path.starts_with("/api/agent/runs/") => {
            let run_id = path.trim_start_matches("/api/agent/runs/");
            match agent_runs_path(state)
                .map_err(|response| response)
                .and_then(|path| {
                    read_agent_run(&path, run_id).map_err(|err| {
                        json_error(500, "agent_runs_unavailable", Some(err.to_string()))
                    })
                }) {
                Ok(Some(run)) => (
                    200,
                    serde_json::to_string(&run).unwrap_or_else(|_| "{}".to_string()),
                ),
                Ok(None) => json_error(
                    404,
                    "agent_run_not_found",
                    Some(format!("agent run `{run_id}` was not found")),
                ),
                Err(response) => response,
            }
        }
        ("GET", "/api/proposals") => match state.daemon.lock() {
            Ok(daemon) => (
                200,
                serde_json::to_string(&daemon.proposals()).unwrap_or_else(|_| "[]".to_string()),
            ),
            Err(_) => json_error(503, "daemon_lock_poisoned", None),
        },
        ("GET", _) if path.starts_with("/api/proposals/") => {
            let proposal_id = path.trim_start_matches("/api/proposals/");
            match state.daemon.lock() {
                Ok(daemon) => match daemon
                    .proposals()
                    .into_iter()
                    .find(|proposal| proposal.proposal_id == proposal_id)
                {
                    Some(proposal) => (
                        200,
                        serde_json::to_string(&proposal).unwrap_or_else(|_| "{}".to_string()),
                    ),
                    None => json_error(
                        404,
                        "proposal_not_found",
                        Some(format!("proposal `{proposal_id}` was not found")),
                    ),
                },
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", "/api/proposals") => {
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: ProposalCreateRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };
            match state.daemon.lock() {
                Ok(mut daemon) => match daemon.create_proposal(request).and_then(|proposal| {
                    daemon.write_runtime_snapshots()?;
                    publish_snapshot_events(&daemon, &state.events);
                    Ok(proposal)
                }) {
                    Ok(proposal) => (
                        200,
                        serde_json::to_string(&proposal).unwrap_or_else(|_| "{}".to_string()),
                    ),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        json_error(404, "deployment_not_found", Some(err.to_string()))
                    }
                    Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                        json_error(400, "invalid_request", Some(err.to_string()))
                    }
                    Err(err) => json_error(500, "proposal_create_failed", Some(err.to_string())),
                },
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("GET", _) if path.starts_with("/api/deployments/") && !path.ends_with("/control") => {
            let deployment_id = path.trim_start_matches("/api/deployments/");
            match state.daemon.lock() {
                Ok(daemon) => match daemon.inspect_deployment(deployment_id) {
                    Some(record) => (
                        200,
                        serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                    ),
                    None => json_error(
                        404,
                        "deployment_not_found",
                        Some(format!("deployment `{deployment_id}` was not found")),
                    ),
                },
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("GET", _) if path.starts_with("/api/trading/diagnose/") => {
            let deployment_id = path.trim_start_matches("/api/trading/diagnose/");
            match state.daemon.lock() {
                Ok(daemon) => {
                    match build_deployment_diagnostics_report(&daemon, state, deployment_id) {
                        Ok(report) => (
                            200,
                            serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {
                            json_error(404, "deployment_not_found", Some(err.to_string()))
                        }
                        Err(err) => {
                            json_error(500, "diagnostics_unavailable", Some(err.to_string()))
                        }
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("PUT", _) if path.starts_with("/api/deployments/") => {
            let deployment_id = path.trim_start_matches("/api/deployments/");
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: DeploymentApplyRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };
            if request.deployment_id != deployment_id {
                return json_error(
                    400,
                    "deployment_id_mismatch",
                    Some(format!(
                        "request deployment_id `{}` did not match path `{deployment_id}`",
                        request.deployment_id
                    )),
                );
            }
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon.apply_deployment(request).and_then(|record| {
                        daemon.write_runtime_snapshots()?;
                        publish_snapshot_events(&daemon, &state.events);
                        Ok(record)
                    }) {
                        Ok(record) => (
                            200,
                            serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                            json_error(400, "apply_failed", Some(err.to_string()))
                        }
                        Err(err) => json_error(500, "apply_failed", Some(err.to_string())),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/control") => {
            let deployment_id = path
                .trim_start_matches("/api/deployments/")
                .trim_end_matches("/control")
                .trim_end_matches('/');
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: DeploymentControlRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .control_deployment(deployment_id, request)
                        .and_then(|record| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(record)
                        }) {
                        Ok(Some(record)) => (
                            200,
                            serde_json::to_string(&record).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Ok(None) => json_error(
                            404,
                            "deployment_not_found",
                            Some(format!("deployment `{deployment_id}` was not found")),
                        ),
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {
                            json_error(404, "deployment_not_found", Some(err.to_string()))
                        }
                        Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                            json_error(400, "invalid_request", Some(err.to_string()))
                        }
                        Err(err) => json_error(500, "control_failed", Some(err.to_string())),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/cancel") => {
            let suffix = path.trim_start_matches("/api/deployments/");
            let Some((deployment_id, order_suffix)) = suffix.split_once("/orders/") else {
                return json_error(404, "not_found", None);
            };
            let order_id = order_suffix
                .trim_end_matches("/cancel")
                .trim_end_matches('/');
            if order_id.is_empty() {
                return json_error(404, "not_found", None);
            }

            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .cancel_order(deployment_id, order_id)
                        .and_then(|response| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(response)
                        }) {
                        Ok(response) => (
                            200,
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => submit_intent_error_response(err, deployment_id),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/replace") => {
            let suffix = path.trim_start_matches("/api/deployments/");
            let Some((deployment_id, order_suffix)) = suffix.split_once("/orders/") else {
                return json_error(404, "not_found", None);
            };
            let order_id = order_suffix
                .trim_end_matches("/replace")
                .trim_end_matches('/');
            if order_id.is_empty() {
                return json_error(404, "not_found", None);
            }
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: OrderReplaceRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };

            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .replace_order(deployment_id, order_id, request)
                        .and_then(|response| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(response)
                        }) {
                        Ok(response) => (
                            200,
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Err(err) => submit_intent_error_response(err, deployment_id),
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/deployments/") && path.ends_with("/intents") => {
            let deployment_id = path
                .trim_start_matches("/api/deployments/")
                .trim_end_matches("/intents")
                .trim_end_matches('/');
            let Some(body) = body else {
                return json_error(400, "missing_body", None);
            };
            let request: PaperIntentRequest = match serde_json::from_str(body) {
                Ok(request) => request,
                Err(err) => return json_error(400, "invalid_json", Some(err.to_string())),
            };
            let side = match trade_side_from_wire(&request.side) {
                Ok(side) => side,
                Err(error) => return json_error(400, &error.error, error.message),
            };
            let Some(intent_source) = intent_source else {
                return json_error(
                    401,
                    "unauthorized",
                    Some(
                        "intent admission requires worker, operator, or admin identity".to_string(),
                    ),
                );
            };

            let prepared = match state.daemon.lock() {
                Ok(mut daemon) => daemon.prepare_intent_idempotent_from(
                    TradingIntent {
                        intent_id: request
                            .idempotency_key
                            .as_deref()
                            .map(|key| format!("request-{key}"))
                            .unwrap_or_else(|| next_paper_intent_id(deployment_id)),
                        deployment_id: deployment_id.to_string(),
                        market_id: request.market_id,
                        token_id: request.token_id,
                        side,
                        quantity: request.quantity,
                        limit_price: request.limit_price,
                        purpose: intent_purpose_from_wire(request.purpose),
                        created_at: chrono::Utc::now(),
                    },
                    request.idempotency_key.as_deref(),
                    intent_source,
                ),
                Err(_) => return json_error(503, "daemon_lock_poisoned", None),
            };
            let response = match prepared {
                Ok(PreparedIntentSubmission::Complete(response)) => Ok(response),
                Ok(PreparedIntentSubmission::Live(prepared)) => {
                    let outcome = prepared.execute();
                    match state.daemon.lock() {
                        Ok(mut daemon) => daemon.finish_prepared_live_intent(prepared, outcome),
                        Err(_) => return json_error(503, "daemon_lock_poisoned", None),
                    }
                }
                Err(error) => Err(error),
            };
            match response {
                Ok(response) => (
                    200,
                    serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string()),
                ),
                Err(err) => submit_intent_error_response(err, deployment_id),
            }
        }
        ("POST", _) if path.starts_with("/api/proposals/") && path.ends_with("/approve") => {
            let proposal_id = path
                .trim_start_matches("/api/proposals/")
                .trim_end_matches("/approve")
                .trim_end_matches('/');
            let request = body
                .map(|raw| serde_json::from_str::<ProposalDecisionRequest>(raw))
                .transpose()
                .map_err(|err| json_error(400, "invalid_json", Some(err.to_string())));
            let request = match request {
                Ok(Some(request)) => request,
                Ok(None) => ProposalDecisionRequest::default(),
                Err(response) => return response,
            };
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .approve_proposal(proposal_id, request)
                        .and_then(|proposal| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(proposal)
                        }) {
                        Ok(Some(proposal)) => (
                            200,
                            serde_json::to_string(&proposal).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Ok(None) => json_error(
                            404,
                            "proposal_not_found",
                            Some(format!("proposal `{proposal_id}` was not found")),
                        ),
                        Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                            json_error(400, "invalid_request", Some(err.to_string()))
                        }
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {
                            json_error(404, "deployment_not_found", Some(err.to_string()))
                        }
                        Err(err) => {
                            json_error(500, "proposal_approve_failed", Some(err.to_string()))
                        }
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        ("POST", _) if path.starts_with("/api/proposals/") && path.ends_with("/reject") => {
            let proposal_id = path
                .trim_start_matches("/api/proposals/")
                .trim_end_matches("/reject")
                .trim_end_matches('/');
            let request = body
                .map(|raw| serde_json::from_str::<ProposalDecisionRequest>(raw))
                .transpose()
                .map_err(|err| json_error(400, "invalid_json", Some(err.to_string())));
            let request = match request {
                Ok(Some(request)) => request,
                Ok(None) => ProposalDecisionRequest::default(),
                Err(response) => return response,
            };
            match state.daemon.lock() {
                Ok(mut daemon) => {
                    match daemon
                        .reject_proposal(proposal_id, request)
                        .and_then(|proposal| {
                            daemon.write_runtime_snapshots()?;
                            publish_snapshot_events(&daemon, &state.events);
                            Ok(proposal)
                        }) {
                        Ok(Some(proposal)) => (
                            200,
                            serde_json::to_string(&proposal).unwrap_or_else(|_| "{}".to_string()),
                        ),
                        Ok(None) => json_error(
                            404,
                            "proposal_not_found",
                            Some(format!("proposal `{proposal_id}` was not found")),
                        ),
                        Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                            json_error(400, "invalid_request", Some(err.to_string()))
                        }
                        Err(err) => {
                            json_error(500, "proposal_reject_failed", Some(err.to_string()))
                        }
                    }
                }
                Err(_) => json_error(503, "daemon_lock_poisoned", None),
            }
        }
        _ => json_error(404, "not_found", None),
    }
}

fn trade_side_from_wire(side: &str) -> Result<TradeSide, ControlPlaneErrorResponse> {
    match side {
        "buy" => Ok(TradeSide::Buy),
        "sell" => Ok(TradeSide::Sell),
        _ => Err(ControlPlaneErrorResponse {
            error: "invalid_side".to_string(),
            message: Some(format!("unsupported side `{side}`")),
        }),
    }
}

fn intent_purpose_from_wire(purpose: IntentPurpose) -> ploy_trading::IntentPurpose {
    match purpose {
        IntentPurpose::Entry => ploy_trading::IntentPurpose::Entry,
        IntentPurpose::Exit => ploy_trading::IntentPurpose::Exit,
        IntentPurpose::Reduce => ploy_trading::IntentPurpose::Reduce,
        IntentPurpose::Hedge => ploy_trading::IntentPurpose::Hedge,
        IntentPurpose::Cancel => ploy_trading::IntentPurpose::Cancel,
    }
}

pub fn snapshot_events(daemon: &PloyDaemon) -> Vec<OperatorEvent> {
    let system = daemon.control_plane.system.status();
    let deployments = daemon.control_plane.deployments.summaries();
    let trading = daemon.trading_state();
    let oversight = compute_oversight_report(&system, &deployments, &trading);
    let proposals = daemon.proposals();
    vec![
        OperatorEvent::Status(StatusUpdate {
            status: system.status.clone(),
        }),
        OperatorEvent::SystemSnapshot(SystemSnapshotEvent { system }),
        OperatorEvent::DeploymentSnapshot(DeploymentSnapshotEvent { deployments }),
        OperatorEvent::TradingSnapshot(TradingSnapshotEvent { trading }),
        OperatorEvent::MetricsSnapshot(MetricsSnapshotEvent {
            metrics: daemon.platform_metrics(),
        }),
        OperatorEvent::AlertSnapshot(AlertSnapshotEvent {
            alerts: daemon.active_alerts(),
        }),
        OperatorEvent::OversightSnapshot(OversightSnapshotEvent { oversight }),
        OperatorEvent::ProposalSnapshot(ProposalSnapshotEvent { proposals }),
    ]
}

pub fn publish_snapshot_events(daemon: &PloyDaemon, broker: &EventBroker) {
    for event in snapshot_events(daemon) {
        broker.publish(event);
    }
}

fn handle_event_stream(mut stream: TcpStream, state: &Arc<AppState>) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
    )?;

    if let Ok(daemon) = state.daemon.lock() {
        for event in snapshot_events(&daemon) {
            write_sse_event(&mut stream, &event)?;
        }
    }

    let receiver = state.events.subscribe();
    loop {
        match receiver.recv_timeout(Duration::from_secs(15)) {
            Ok(event) => write_sse_event(&mut stream, &event)?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                write!(stream, ": keep-alive\n\n")?;
                stream.flush()?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn write_sse_event(stream: &mut TcpStream, event: &OperatorEvent) -> io::Result<()> {
    let body = serde_json::to_string(event)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    write!(stream, "data: {body}\n\n")?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::{
        access_allowed, admin_session_cookie, append_audit_entry, append_jsonl, client_ip,
        content_length, handle_api_request, handle_authenticated_runtime_request,
        handle_connection, handle_runtime_request, header_end_offset, intent_admission_source,
        rate_limit_key, report_json_response, request_auth_level, required_access,
        response_headers, route_request, snapshot_events, AppState, AuthLevel, RateLimiter,
        ADMIN_SESSION_COOKIE_NAME,
    };
    use crate::events::EventBroker;
    use crate::test_support::StaticExecutionGateway;
    use chrono::{Duration, Utc};
    use ploy_connectivity::{
        CancellationOutcome, CancellationRequest, ExecutionError, ExecutionOutcome,
        ExecutionRequest, LiveExecutionGateway, ReplaceOutcome, ReplaceRequest, TrackedOrder,
    };
    use ploy_operator_contracts::AuditLogEntry;
    use ploy_operator_contracts::{OrderReplaceRequest, PaperIntentRequest};
    use ploy_platform_runtime::runtime_support::IntentAdmissionSource;
    use ploy_strategy_bundles::strategies::three_layer::ThreeLayerConfig;
    use ploy_strategy_bundles::{
        Feed, LiveFeed, MarketUpdate, StrategyDecision, StrategyLogic, ThreeLayerProfile,
        ThreeLayerStrategy,
    };
    use ploy_trading::{
        IntentPurpose as TradingIntentPurpose, OrderLedger, PositionLedger, TradeSide,
        TradingIntent,
    };
    use std::collections::VecDeque;
    use std::fs;
    use std::io::{self, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("duration")
            .as_nanos();
        std::env::temp_dir().join(format!("ployd-http-{label}-{unique}"))
    }

    fn read_response_body(stream: &mut TcpStream) -> String {
        let mut headers = Vec::new();
        let mut byte = [0_u8; 1];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("read response headers");
            headers.push(byte[0]);
        }
        let headers = String::from_utf8(headers).expect("response headers utf8");
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().expect("content length"))
            })
            .expect("content length header");
        let mut body = vec![0_u8; content_length];
        stream.read_exact(&mut body).expect("read response body");
        String::from_utf8(body).expect("response body utf8")
    }

    fn percentile_micros(samples: &mut [u64], per_thousand: usize) -> u64 {
        samples.sort_unstable();
        samples[(samples.len() - 1) * per_thousand / 1_000]
    }

    #[derive(Debug)]
    struct BlockingSubmitGateway {
        entered: Mutex<Option<mpsc::Sender<()>>>,
        release: Mutex<mpsc::Receiver<()>>,
        submits: Arc<AtomicUsize>,
    }

    #[derive(Debug)]
    struct BenchmarkSubmitGateway {
        entered: Arc<Mutex<Vec<Instant>>>,
        submits: AtomicUsize,
    }

    impl LiveExecutionGateway for BenchmarkSubmitGateway {
        fn probe(&self) -> Result<(), ExecutionError> {
            Ok(())
        }

        fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
            self.entered
                .lock()
                .expect("entered lock")
                .push(Instant::now());
            let sequence = self.submits.fetch_add(1, Ordering::SeqCst);
            Ok(ExecutionOutcome::Acknowledged {
                venue_order_id: format!("venue-benchmark-{sequence}"),
            })
        }

        fn cancel(
            &self,
            _request: &CancellationRequest,
        ) -> Result<CancellationOutcome, ExecutionError> {
            Ok(CancellationOutcome::Canceled)
        }

        fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
            unreachable!("replace is not used")
        }

        fn reconcile_fills(
            &self,
            _tracked_orders: &[TrackedOrder],
        ) -> Result<Vec<ploy_trading::FillRecord>, ExecutionError> {
            Ok(Vec::new())
        }
    }

    impl LiveExecutionGateway for BlockingSubmitGateway {
        fn probe(&self) -> Result<(), ExecutionError> {
            Ok(())
        }

        fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            if let Some(entered) = self.entered.lock().expect("entered lock").take() {
                let _ = entered.send(());
            }
            self.release
                .lock()
                .expect("release lock")
                .recv()
                .expect("release submit");
            Ok(ExecutionOutcome::Acknowledged {
                venue_order_id: "venue-blocking".to_string(),
            })
        }

        fn cancel(
            &self,
            _request: &CancellationRequest,
        ) -> Result<CancellationOutcome, ExecutionError> {
            Ok(CancellationOutcome::Canceled)
        }

        fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
            unreachable!("replace is not used")
        }

        fn reconcile_fills(
            &self,
            _tracked_orders: &[TrackedOrder],
        ) -> Result<Vec<ploy_trading::FillRecord>, ExecutionError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn request_reader_helpers_accept_large_content_length() {
        let headers = b"POST /api/proposals HTTP/1.1\r\nHost: localhost\r\nContent-Length: 4096";
        assert_eq!(content_length(headers).expect("content length"), 4096);
    }

    #[test]
    fn report_json_error_preserves_control_plane_envelope() {
        for error in [
            "market_data_health_unavailable",
            "dry_run_summary_unavailable",
        ] {
            let (status, body) = report_json_response(Err("database offline".to_string()), error);
            let payload: serde_json::Value = serde_json::from_str(&body).expect("JSON envelope");

            assert_eq!(status, 500);
            assert_eq!(payload["error"], error);
            assert_eq!(payload["message"], "database offline");
        }
    }

    #[test]
    fn report_json_success_is_returned_without_rewriting() {
        let body = "{\"generated_at\":\"2026-04-29T00:00:00Z\"}".to_string();
        assert_eq!(
            report_json_response(Ok(body.clone()), "unused"),
            (200, body)
        );
    }

    #[test]
    fn request_reader_helpers_find_header_end_after_large_headers() {
        let request = format!(
            "POST /api/proposals HTTP/1.1\r\nX-Large: {}\r\n\r\n{{}}",
            "x".repeat(3000)
        );
        assert!(header_end_offset(request.as_bytes()).is_some());
    }

    #[test]
    fn server_handles_multiple_requests_on_one_keep_alive_connection() {
        let root = temp_dir("server-keep-alive");
        let runtime_root = root.join("run/platform");
        let config = crate::config::PlatformConfig {
            runtime_root: runtime_root.clone(),
            registry_file: root.join("data/state/deployments.json"),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let server_state = Arc::clone(&state);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            handle_connection(stream, &server_state).expect("serve connection");
        });
        let mut client = TcpStream::connect(addr).expect("connect");

        for _ in 0..2 {
            write!(
                client,
                "GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: keep-alive\r\n\r\n"
            )
            .expect("write request");
            assert!(read_response_body(&mut client).contains("\"status\""));
        }
        drop(client);
        server.join().expect("server thread");
    }

    #[test]
    #[ignore = "local no-live-order latency benchmark"]
    fn live_submit_latency_benchmark() {
        const SAMPLES: usize = 1_001;
        let root = temp_dir("live-submit-latency");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([{
                "deployment_id":"benchmark.live",
                "bundle_id":"benchmark",
                "runtime_mode":"live",
                "account_id":"0x1111111111111111111111111111111111111111",
                "max_gross_exposure":"1000000",
                "desired_state":"running",
                "observed_state":"running"
            }])
            .to_string(),
        )
        .expect("registry");
        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            audit_log_file: runtime_root.join("audit-log.jsonl"),
            worker_token: Some(secrecy::SecretString::from("benchmark-worker".to_string())),
            request_rate_limit_per_minute: 0,
            ..crate::config::PlatformConfig::default()
        };
        crate::runtime::seed_empty_live_ledgers(&config);
        let entered = Arc::new(Mutex::new(Vec::with_capacity(SAMPLES)));
        let mut daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(BenchmarkSubmitGateway {
                entered: Arc::clone(&entered),
                submits: AtomicUsize::new(0),
            }),
        )
        .expect("boot daemon");
        daemon.control_plane.deployments.set_observed_state(
            "benchmark.live",
            ploy_operator_contracts::ObservedState::Running,
        );
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let server_state = Arc::clone(&state);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            handle_connection(stream, &server_state).expect("serve benchmark connection");
        });
        let mut client = TcpStream::connect(addr).expect("connect");
        client.set_nodelay(true).expect("nodelay");
        let base_ts = Utc::now();
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let mut strategy = ThreeLayerStrategy::new(ThreeLayerConfig {
            symbols: vec!["BTCUSDT".to_string()],
            profile: ThreeLayerProfile::Mixed,
            min_direction_prob: 0.5,
            min_distance_over_sigma: 0.0,
            min_confirmation_score: 0.0,
            require_confirmation: false,
            min_drift_confirmation: 0.0,
            min_edge: 0.0,
            min_reward_risk: 0.0,
            alpha_contrarian: false,
            cex_contrarian: false,
            probability_shrink: 1.0,
            probability_haircut: 0.0,
            take_profit_ask: 0.70,
            stop_distance_pct: 0.02,
            max_pm_lag_secs: 15,
            min_time_remaining_secs: 1,
            max_time_remaining_secs: 300,
            cooldown_secs: 0,
            stake_usd: rust_decimal::Decimal::new(25, 0),
            max_positions: 10,
            max_daily_trades: u32::MAX,
            allowed_window_secs: vec![300],
            min_entry_price: 0.01,
            max_entry_price: 0.99,
            min_entry_score: 0.0,
            autofactor_runtime_score: None,
            event_ml_model: None,
            visible_depth_haircut: rust_decimal::Decimal::ONE,
            max_sweep_levels: 0,
            max_sweep_price_delta: rust_decimal::Decimal::ZERO,
        });
        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: Arc::from("market-1"),
                symbol: Arc::from("BTCUSDT"),
                up_token: Arc::from("token-1"),
                down_token: Arc::from("token-2"),
                end_time: base_ts + Duration::seconds(300),
                window_secs: 300,
                price_to_beat: Some(rust_decimal::Decimal::new(100_000, 0)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: rust_decimal::Decimal::new(100_000, 0),
                ts: base_ts,
            },
            &positions,
            &orders,
        );
        let (tick_tx, tick_rx) = tokio::sync::broadcast::channel(16);
        let mut tick_feed = LiveFeed::new(tick_rx);
        let tick_runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tick runtime");
        let mut canonical_tick_to_decision = Vec::with_capacity(SAMPLES);
        let mut canonical_tick_to_gateway = Vec::with_capacity(SAMPLES);
        let mut canonical_tick_to_response = Vec::with_capacity(SAMPLES);
        let mut client_to_gateway = Vec::with_capacity(SAMPLES);
        let mut gateway_to_response = Vec::with_capacity(SAMPLES);
        let mut end_to_end = Vec::with_capacity(SAMPLES);

        for sequence in 0..SAMPLES {
            let tick = MarketUpdate::Quote {
                token_id: Arc::from("token-1"),
                bid: Some(rust_decimal::Decimal::new(19, 2)),
                ask: Some(rust_decimal::Decimal::new(20, 2)),
                bid_size: Some(rust_decimal::Decimal::new(1_000, 0)),
                ask_size: Some(rust_decimal::Decimal::new(1_000, 0)),
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: base_ts + Duration::milliseconds(sequence as i64),
            };
            let tick_started = Instant::now();
            tick_tx.send(tick).expect("broadcast canonical tick");
            let tick = tick_runtime
                .block_on(tick_feed.next())
                .expect("receive canonical tick");
            let decisions = strategy.on_update(&tick, &positions, &orders);
            let decision_at = Instant::now();
            let intent = match decisions.as_slice() {
                [StrategyDecision::Enter { intent, .. }] => intent,
                other => panic!("benchmark tick must emit one entry, got {other:?}"),
            };
            let body = serde_json::to_string(&PaperIntentRequest {
                idempotency_key: Some(intent.intent_id.clone()),
                market_id: intent.market_id.clone(),
                token_id: intent.token_id.clone(),
                side: match intent.side {
                    TradeSide::Buy => "buy",
                    TradeSide::Sell => "sell",
                }
                .to_string(),
                quantity: intent.quantity,
                limit_price: intent.limit_price,
                purpose: ploy_operator_contracts::IntentPurpose::Entry,
            })
            .expect("request json");
            let started = Instant::now();
            write!(
                client,
                "POST /api/deployments/benchmark.live/intents HTTP/1.1\r\nHost: {addr}\r\nx-ploy-worker-token: benchmark-worker\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write request");
            let response = read_response_body(&mut client);
            let completed = Instant::now();
            assert!(response.contains("\"state\":\"acknowledged\""));
            let wire = entered.lock().expect("entered lock")[sequence];
            canonical_tick_to_decision
                .push(decision_at.duration_since(tick_started).as_micros() as u64);
            canonical_tick_to_gateway.push(wire.duration_since(tick_started).as_micros() as u64);
            canonical_tick_to_response
                .push(completed.duration_since(tick_started).as_micros() as u64);
            client_to_gateway.push(wire.duration_since(started).as_micros() as u64);
            gateway_to_response.push(completed.duration_since(wire).as_micros() as u64);
            end_to_end.push(completed.duration_since(started).as_micros() as u64);
        }

        println!(
            "live-submit-latency-us canonical_tick_to_decision[p50={},p99={},p999={}] canonical_tick_to_gateway[p50={},p99={},p999={}] canonical_tick_to_response[p50={},p99={},p999={}] client_to_gateway[p50={},p99={},p999={}] gateway_to_response[p50={},p99={},p999={}] end_to_end[p50={},p99={},p999={}] samples={SAMPLES}",
            percentile_micros(&mut canonical_tick_to_decision, 500),
            percentile_micros(&mut canonical_tick_to_decision, 990),
            percentile_micros(&mut canonical_tick_to_decision, 999),
            percentile_micros(&mut canonical_tick_to_gateway, 500),
            percentile_micros(&mut canonical_tick_to_gateway, 990),
            percentile_micros(&mut canonical_tick_to_gateway, 999),
            percentile_micros(&mut canonical_tick_to_response, 500),
            percentile_micros(&mut canonical_tick_to_response, 990),
            percentile_micros(&mut canonical_tick_to_response, 999),
            percentile_micros(&mut client_to_gateway, 500),
            percentile_micros(&mut client_to_gateway, 990),
            percentile_micros(&mut client_to_gateway, 999),
            percentile_micros(&mut gateway_to_response, 500),
            percentile_micros(&mut gateway_to_response, 990),
            percentile_micros(&mut gateway_to_response, 999),
            percentile_micros(&mut end_to_end, 500),
            percentile_micros(&mut end_to_end, 990),
            percentile_micros(&mut end_to_end, 999),
        );
        drop(client);
        server.join().expect("server thread");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn route_request_serves_system_and_deployment_snapshots() {
        let runtime_root = temp_dir("routes");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-status.json"),
            serde_json::json!({
                "status": "running",
                "uptime_seconds": 7,
                "version": "0.1.0",
                "strategy": "platform",
                "last_trade_time": null,
                "websocket_connected": false,
                "database_connected": false,
                "error_count_1h": 0,
                "live_reconcile_failures": 0,
                "next_live_reconcile_at": null,
                "last_live_reconcile_error": null
            })
            .to_string(),
        )
        .expect("write status");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("write deployments");

        let (status_code, status_body) = route_request("/api/system/status", &runtime_root);
        assert_eq!(status_code, 200);
        assert!(status_body.contains("\"status\":\"running\""));

        let (deployments_code, deployments_body) = route_request("/api/deployments", &runtime_root);
        assert_eq!(deployments_code, 200);
        assert!(deployments_body.contains("\"deployment_id\":\"example.paper\""));
    }

    #[test]
    fn rate_limiter_denies_requests_after_limit_within_window() {
        let mut limiter = RateLimiter::default();
        assert!(limiter.allow("127.0.0.1|admin|GET|/api/deployments", 1));
        assert!(!limiter.allow("127.0.0.1|admin|GET|/api/deployments", 1));
    }

    #[test]
    fn same_ip_different_ports_share_rate_limit() {
        let mut limiter = RateLimiter::default();
        let first = rate_limit_key(
            Some("127.0.0.1:41000"),
            AuthLevel::None,
            "GET",
            "/api/deployments",
        );
        let second = rate_limit_key(
            Some("127.0.0.1:42000"),
            AuthLevel::None,
            "GET",
            "/api/deployments",
        );

        assert!(limiter.allow(&first, 1));
        assert!(!limiter.allow(&second, 1));
    }

    #[test]
    fn different_paths_share_rate_limit() {
        let mut limiter = RateLimiter::default();
        let first = rate_limit_key(
            Some("127.0.0.1:41000"),
            AuthLevel::Admin,
            "GET",
            "/api/deployments",
        );
        let second = rate_limit_key(
            Some("127.0.0.1:41000"),
            AuthLevel::Admin,
            "POST",
            "/api/proposals",
        );

        assert!(limiter.allow(&first, 1));
        assert!(!limiter.allow(&second, 1));
    }

    #[test]
    fn trusted_loopback_proxy_uses_real_client_ip() {
        let request = "GET /auth/login HTTP/1.1\r\nX-Real-IP: 203.0.113.9\r\n\r\n";

        assert_eq!(client_ip(Some("127.0.0.1:41000"), request), "203.0.113.9");
        assert_eq!(
            client_ip(Some("198.51.100.7:41000"), request),
            "198.51.100.7"
        );
    }

    #[test]
    fn expired_rate_limit_buckets_are_removed() {
        let mut limiter = RateLimiter::default();
        limiter.requests.insert(
            "expired|none".to_string(),
            VecDeque::from([Instant::now() - StdDuration::from_secs(61)]),
        );

        assert!(limiter.allow("current|none", 1));
        assert!(!limiter.requests.contains_key("expired|none"));
    }

    #[test]
    fn missing_tokens_do_not_authorize_protected_routes() {
        assert!(access_allowed(
            AuthLevel::None,
            super::RequiredAccess::Public,
            false,
            false,
            false,
            false,
        ));
        for required in [
            super::RequiredAccess::ReadOnly,
            super::RequiredAccess::Operator,
            super::RequiredAccess::Admin,
        ] {
            assert!(!access_allowed(
                AuthLevel::None,
                required,
                false,
                false,
                false,
                false,
            ));
        }
    }

    #[test]
    fn auth_session_does_not_report_protected_apis_open_without_tokens() {
        let config = crate::config::PlatformConfig::default();
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (code, body) = handle_authenticated_runtime_request(
            "GET",
            "/auth/session",
            None,
            AuthLevel::None,
            false,
            false,
            false,
            false,
            &state,
        );

        assert_eq!(code, 200);
        assert!(body.contains("\"auth_required\":true"));
        assert!(body.contains("\"authenticated\":false"));
    }

    #[test]
    fn handle_runtime_request_reads_recent_audit_entries() {
        let root = temp_dir("audit-read");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        let audit_log_file = runtime_root.join("audit-log.jsonl");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root,
            status_file: root.join("run/platform/system-status.json"),
            deployment_status_file: root.join("run/platform/deployments.json"),
            trading_state_file: root.join("run/platform/trading-state.json"),
            audit_log_file: audit_log_file.clone(),
            ..crate::config::PlatformConfig::default()
        };
        append_audit_entry(
            &audit_log_file,
            &AuditLogEntry {
                timestamp: Utc::now(),
                method: "POST".to_string(),
                path: "/api/deployments/example.paper/control".to_string(),
                client_addr: Some("127.0.0.1:9000".to_string()),
                auth_level: "admin".to_string(),
                required_access: "admin".to_string(),
                status_code: 200,
                outcome: "allowed".to_string(),
                message: Some("deployment paused".to_string()),
            },
        )
        .expect("append audit");

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) = handle_runtime_request("GET", "/api/audit/logs", None, &state);
        assert_eq!(status_code, 200);
        assert!(body.contains("/api/deployments/example.paper/control"));
        assert!(body.contains("deployment paused"));
    }

    #[test]
    fn auth_session_reports_auth_requirement_when_admin_token_is_configured() {
        let config = crate::config::PlatformConfig {
            admin_token: Some("secret-token".to_string().into()),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (code, body) = handle_authenticated_runtime_request(
            "GET",
            "/auth/session",
            None,
            AuthLevel::None,
            true,
            false,
            false,
            false,
            &state,
        );

        assert_eq!(code, 200);
        assert!(body.contains("\"auth_required\":true"));
        assert!(body.contains("\"authenticated\":false"));
    }

    #[test]
    fn unauthorized_requests_are_rejected_when_admin_token_is_configured() {
        let config = crate::config::PlatformConfig {
            admin_token: Some("secret-token".to_string().into()),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (code, body) = handle_authenticated_runtime_request(
            "POST",
            "/api/deployments/example.paper/control",
            Some("{\"desired_state\":\"paused\",\"deployment_state\":null}"),
            AuthLevel::None,
            true,
            false,
            false,
            false,
            &state,
        );

        assert_eq!(code, 401);
        assert!(body.contains("\"error\":\"unauthorized\""));
    }

    #[test]
    fn auth_login_accepts_matching_admin_token() {
        let config = crate::config::PlatformConfig {
            admin_token: Some("secret-token".to_string().into()),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (code, body) = handle_authenticated_runtime_request(
            "POST",
            "/auth/login",
            Some("{\"admin_token\":\"secret-token\"}"),
            AuthLevel::None,
            true,
            false,
            false,
            false,
            &state,
        );

        assert_eq!(code, 200);
        assert!(body.contains("\"success\":true"));
    }

    #[test]
    fn auth_login_rejects_when_admin_auth_is_not_configured() {
        let config = crate::config::PlatformConfig::default();
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (code, body) = handle_authenticated_runtime_request(
            "POST",
            "/auth/login",
            Some("{\"admin_token\":\"anything\"}"),
            AuthLevel::None,
            false,
            false,
            false,
            false,
            &state,
        );

        assert_eq!(code, 503);
        assert!(body.contains("\"error\":\"admin_auth_not_configured\""));
        assert!(response_headers("POST", "/auth/login", code, None, "cookie-secret").is_empty());
    }

    #[test]
    fn request_auth_level_accepts_admin_session_cookie() {
        let cookie = admin_session_cookie("secret-token", "cookie-secret");
        let request = format!(
            "GET /api/events/stream HTTP/1.1\r\nHost: 127.0.0.1:8081\r\nCookie: {cookie}; theme=dark\r\n\r\n"
        );
        assert_eq!(
            request_auth_level(
                &request,
                Some("secret-token"),
                None,
                None,
                None,
                "cookie-secret",
            ),
            AuthLevel::Admin
        );
    }

    #[test]
    fn auth_responses_emit_session_cookie_headers() {
        let login_headers = response_headers(
            "POST",
            "/auth/login",
            200,
            Some("secret-token"),
            "cookie-secret",
        );
        assert!(login_headers
            .iter()
            .any(|(name, value)| name == "Set-Cookie"
                && value.contains(&format!("{ADMIN_SESSION_COOKIE_NAME}=v1."))
                && !value.contains("secret-token")));

        let logout_headers = response_headers(
            "POST",
            "/auth/logout",
            200,
            Some("secret-token"),
            "cookie-secret",
        );
        assert!(logout_headers
            .iter()
            .any(|(name, value)| name == "Set-Cookie"
                && value.contains(&format!("{ADMIN_SESSION_COOKIE_NAME}="))
                && value.contains("Max-Age=0")));
    }

    #[test]
    fn signed_session_cookie_does_not_authenticate_other_tokens() {
        let cookie = admin_session_cookie("secret-token", "cookie-secret");
        let request = format!("GET / HTTP/1.1\r\nCookie: {cookie}\r\n\r\n");
        assert_eq!(
            request_auth_level(
                &request,
                Some("other-token"),
                None,
                None,
                None,
                "cookie-secret",
            ),
            AuthLevel::None
        );
    }

    #[test]
    fn sidecar_token_grants_read_only_access_but_not_admin_access() {
        let request =
            "GET /api/deployments HTTP/1.1\r\nx-ploy-sidecar-token: sidecar-secret\r\n\r\n";
        assert_eq!(
            request_auth_level(
                request,
                Some("admin-secret"),
                None,
                None,
                Some("sidecar-secret"),
                "cookie-secret"
            ),
            AuthLevel::Sidecar
        );

        let config = crate::config::PlatformConfig {
            admin_token: Some("admin-secret".to_string().into()),
            sidecar_token: Some("sidecar-secret".to_string().into()),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (read_code, _) = handle_authenticated_runtime_request(
            "GET",
            "/api/deployments",
            None,
            AuthLevel::Sidecar,
            true,
            false,
            false,
            true,
            &state,
        );
        assert_eq!(read_code, 200);

        let (write_code, write_body) = handle_authenticated_runtime_request(
            "POST",
            "/api/deployments/example.paper/control",
            Some("{\"desired_state\":\"paused\",\"deployment_state\":null}"),
            AuthLevel::Sidecar,
            true,
            false,
            false,
            true,
            &state,
        );
        assert_eq!(write_code, 401);
        assert!(write_body.contains("\"error\":\"unauthorized\""));
    }

    #[test]
    fn operator_token_grants_write_access_but_not_admin_access() {
        let request = "POST /api/deployments/example.paper/control HTTP/1.1\r\nx-ploy-operator-token: operator-secret\r\n\r\n";
        assert_eq!(
            request_auth_level(
                request,
                Some("admin-secret"),
                Some("operator-secret"),
                None,
                Some("sidecar-secret"),
                "cookie-secret"
            ),
            AuthLevel::Operator
        );

        let config = crate::config::PlatformConfig {
            admin_token: Some("admin-secret".to_string().into()),
            operator_token: Some("operator-secret".to_string().into()),
            sidecar_token: Some("sidecar-secret".to_string().into()),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (write_code, write_body) = handle_authenticated_runtime_request(
            "POST",
            "/api/deployments/example.paper/control",
            Some("{\"desired_state\":\"paused\",\"deployment_state\":null}"),
            AuthLevel::Operator,
            true,
            true,
            false,
            true,
            &state,
        );
        assert_eq!(write_code, 404);
        assert!(write_body.contains("deployment_not_found"));

        let (audit_code, audit_body) = handle_authenticated_runtime_request(
            "GET",
            "/api/audit/logs",
            None,
            AuthLevel::Operator,
            true,
            true,
            false,
            true,
            &state,
        );
        assert_eq!(audit_code, 401);
        assert!(audit_body.contains("\"error\":\"unauthorized\""));
    }

    #[test]
    fn worker_token_cannot_access_operator_or_admin_endpoints() {
        let request = "POST /api/deployments/example.live/intents HTTP/1.1\r\nx-ploy-worker-token: worker-secret\r\n\r\n";
        let auth_level = request_auth_level(
            request,
            Some("admin-secret"),
            Some("operator-secret"),
            Some("worker-secret"),
            Some("sidecar-secret"),
            "cookie-secret",
        );
        assert_eq!(auth_level, AuthLevel::Worker);
        assert!(access_allowed(
            auth_level,
            required_access("POST", "/api/deployments/example.live/intents"),
            true,
            true,
            true,
            true,
        ));
        assert!(!access_allowed(
            auth_level,
            required_access("POST", "/api/deployments/example.live/control"),
            true,
            true,
            true,
            true,
        ));
        assert!(!access_allowed(
            auth_level,
            required_access("GET", "/api/audit/logs"),
            true,
            true,
            true,
            true,
        ));
    }

    #[test]
    fn intent_json_cannot_spoof_admission_source() {
        let request: PaperIntentRequest = serde_json::from_value(serde_json::json!({
            "market_id": "market-1",
            "token_id": "token-1",
            "side": "sell",
            "quantity": "1",
            "limit_price": "0.5",
            "purpose": "reduce",
            "admission_source": "authenticated_operator"
        }))
        .expect("intent request accepts only its public trading fields");

        assert_eq!(
            request.purpose,
            ploy_operator_contracts::IntentPurpose::Reduce
        );
        assert_eq!(
            intent_admission_source(AuthLevel::Worker),
            Some(IntentAdmissionSource::Worker),
        );
    }

    #[test]
    fn unauthenticated_and_sidecar_cannot_become_operator_source() {
        assert_eq!(intent_admission_source(AuthLevel::None), None);
        assert_eq!(intent_admission_source(AuthLevel::Sidecar), None);
        assert_eq!(
            intent_admission_source(AuthLevel::Operator),
            Some(IntentAdmissionSource::AuthenticatedOperator),
        );
        assert_eq!(
            intent_admission_source(AuthLevel::Admin),
            Some(IntentAdmissionSource::AuthenticatedOperator),
        );
    }

    #[test]
    fn worker_only_auth_configuration_is_recognized() {
        let config = crate::config::PlatformConfig {
            worker_token: Some("worker-secret".to_string().into()),
            ..crate::config::PlatformConfig::default()
        };
        let daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });
        let body = serde_json::json!({
            "market_id": "market-1",
            "token_id": "token-1",
            "side": "buy",
            "quantity": "1",
            "limit_price": "0.5",
            "purpose": "entry"
        })
        .to_string();

        let (missing_code, missing_body) = handle_authenticated_runtime_request(
            "POST",
            "/api/deployments/missing.live/intents",
            Some(&body),
            AuthLevel::None,
            false,
            false,
            true,
            false,
            &state,
        );
        assert_eq!(missing_code, 401);
        assert!(missing_body.contains("worker, operator, or admin token is required"));

        let (worker_code, worker_body) = handle_authenticated_runtime_request(
            "POST",
            "/api/deployments/missing.live/intents",
            Some(&body),
            AuthLevel::Worker,
            false,
            false,
            true,
            false,
            &state,
        );
        assert_eq!(worker_code, 404);
        assert!(worker_body.contains("deployment_not_found"));
    }

    #[test]
    fn handle_api_request_applies_and_controls_deployments() {
        let root = temp_dir("apply");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(&registry_file, "[]").expect("empty registry");

        let config = crate::config::PlatformConfig {
            registry_file: registry_file.clone(),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let invalid_body = serde_json::json!({
            "deployment_id": "invalid.paper",
            "bundle_id": "example",
            "runtime_mode": "paper",
            "account_id": "acct-invalid",
            "desired_state": "running"
        })
        .to_string();
        let (invalid_code, invalid_response) = handle_api_request(
            "PUT",
            "/api/deployments/invalid.paper",
            Some(&invalid_body),
            &config,
        );
        assert_eq!(invalid_code, 400);
        assert!(invalid_response.contains("apply_failed"));

        let apply_body = serde_json::json!({
            "deployment_id": "example.paper",
            "bundle_id": "example",
            "runtime_mode": "paper",
            "account_id": "paper:test-http-apply",
            "desired_state": "running"
        })
        .to_string();
        let (apply_code, apply_response) = handle_api_request(
            "PUT",
            "/api/deployments/example.paper",
            Some(&apply_body),
            &config,
        );
        assert_eq!(apply_code, 200);
        assert!(apply_response.contains("\"deployment_id\":\"example.paper\""));

        let (inspect_code, inspect_response) =
            handle_api_request("GET", "/api/deployments/example.paper", None, &config);
        assert_eq!(inspect_code, 200);
        assert!(inspect_response.contains("\"bundle_id\":\"example\""));

        let control_body = serde_json::json!({
            "desired_state": "paused"
        })
        .to_string();
        let (control_code, control_response) = handle_api_request(
            "POST",
            "/api/deployments/example.paper/control",
            Some(&control_body),
            &config,
        );
        assert_eq!(control_code, 200);
        assert!(control_response.contains("\"desired_state\":\"paused\""));
    }

    #[test]
    fn handle_api_request_submits_paper_intent() {
        let root = temp_dir("submit-intent");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "account_id": "paper:test-http-submit",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file: registry_file.clone(),
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let body = serde_json::to_string(&PaperIntentRequest {
            idempotency_key: None,
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::new(5, 1)),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");

        let (submit_code, submit_response) = handle_api_request(
            "POST",
            "/api/deployments/example.paper/intents",
            Some(&body),
            &config,
        );
        assert_eq!(submit_code, 200);
        assert!(submit_response.contains("\"deployment_id\":\"example.paper\""));
    }

    #[test]
    fn route_request_serves_trading_state_snapshot() {
        let runtime_root = temp_dir("trading-routes");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("trading-state.json"),
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "runtime_mode": "paper",
                    "intents": [],
                    "orders": [],
                    "fills": [],
                    "positions": [],
                    "pnl": {
                        "realized_pnl": "0",
                        "unrealized_pnl": "0",
                        "total_fees": "0",
                        "net_pnl": "0"
                    },
                    "risk": {
                        "pending_intents": 0,
                        "active_orders": 0,
                        "open_positions": 0,
                        "gross_exposure": "0",
                        "reserved_order_exposure": "0",
                        "total_gross_exposure": "0"
                    }
                }
            ])
            .to_string(),
        )
        .expect("write trading state");

        let (status_code, body) = route_request("/api/trading/state", &runtime_root);
        assert_eq!(status_code, 200);
        assert!(body.contains("\"deployment_id\":\"example.paper\""));
    }

    #[test]
    fn handle_runtime_request_serves_metrics_and_alert_snapshots() {
        let root = temp_dir("metrics-alerts");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let mut daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("unused")),
        )
        .expect("boot daemon");
        daemon.control_plane.system.note_source_failure(
            "live_reconcile",
            "live_reconcile",
            Duration::seconds(15),
            "live reconcile loop exceeded stale threshold".to_string(),
        );
        daemon.control_plane.system.note_source_failure(
            "venue:polymarket",
            "venue",
            Duration::seconds(15),
            "venue heartbeat exceeded stale threshold".to_string(),
        );
        daemon.control_plane.system.refresh_source_health();

        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (metrics_code, metrics_body) =
            handle_runtime_request("GET", "/api/system/metrics", None, &state);
        assert_eq!(metrics_code, 200);
        let metrics: ploy_operator_contracts::PlatformMetrics =
            serde_json::from_str(&metrics_body).expect("metrics json");
        assert!(metrics.active_alerts >= 1);
        assert!(metrics.stale_sources >= 1);

        let (alerts_code, alerts_body) =
            handle_runtime_request("GET", "/api/system/alerts", None, &state);
        assert_eq!(alerts_code, 200);
        let alerts: Vec<ploy_operator_contracts::ActiveAlert> =
            serde_json::from_str(&alerts_body).expect("alerts json");
        assert!(alerts
            .iter()
            .any(|alert| alert.kind == ploy_operator_contracts::AlertKind::SourceStale));
        assert!(alerts
            .iter()
            .any(|alert| alert.source_id.contains("live_reconcile")));
    }

    #[test]
    fn snapshot_events_include_control_plane_and_trading_payloads() {
        let daemon = crate::runtime::PloyDaemon::boot(&crate::config::PlatformConfig::default())
            .expect("boot daemon");
        let events = snapshot_events(&daemon);
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::SystemSnapshot(_)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::DeploymentSnapshot(_)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::TradingSnapshot(_)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::MetricsSnapshot(_)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::AlertSnapshot(_)
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ploy_operator_contracts::OperatorEvent::OversightSnapshot(_)
        )));
    }

    #[test]
    fn handle_runtime_request_submits_live_intent_via_shared_daemon_state() {
        let root = temp_dir("runtime-live-intent");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": "0x1111111111111111111111111111111111111111",
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        crate::runtime::seed_empty_live_ledgers(&config);
        let mut daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("venue-live-http-1")),
        )
        .expect("boot daemon");
        daemon.control_plane.deployments.set_observed_state(
            "example.live",
            ploy_operator_contracts::ObservedState::Running,
        );
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let body = serde_json::to_string(&PaperIntentRequest {
            idempotency_key: None,
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::new(5, 1)),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");

        let (submit_code, submit_response) = handle_runtime_request(
            "POST",
            "/api/deployments/example.live/intents",
            Some(&body),
            &state,
        );
        assert_eq!(submit_code, 200);
        assert!(submit_response.contains("\"state\":\"acknowledged\""));

        let trading_body =
            fs::read_to_string(runtime_root.join("trading-state.json")).expect("trading snapshot");
        let trading: serde_json::Value =
            serde_json::from_str(&trading_body).expect("snapshot json");
        assert_eq!(trading[0]["deployment_id"], "example.live");
        assert_eq!(
            trading[0]["orders"][0]["venue_order_id"],
            "venue-live-http-1"
        );
    }

    #[test]
    fn live_venue_submit_does_not_hold_the_daemon_mutex() {
        let root = temp_dir("runtime-live-unlocked-submit");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([{
                "deployment_id":"example.live",
                "bundle_id":"example",
                "runtime_mode":"live",
                "account_id":"0x1111111111111111111111111111111111111111",
                "max_gross_exposure":"5",
                "desired_state":"running",
                "observed_state":"running"
            }])
            .to_string(),
        )
        .expect("registry");
        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };
        crate::runtime::seed_empty_live_ledgers(&config);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let submits = Arc::new(AtomicUsize::new(0));
        let mut daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(BlockingSubmitGateway {
                entered: Mutex::new(Some(entered_tx)),
                release: Mutex::new(release_rx),
                submits: Arc::clone(&submits),
            }),
        )
        .expect("boot daemon");
        daemon.control_plane.deployments.set_observed_state(
            "example.live",
            ploy_operator_contracts::ObservedState::Running,
        );
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });
        let body = serde_json::to_string(&PaperIntentRequest {
            idempotency_key: Some("blocking-1".to_string()),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::new(5, 1)),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");
        let retry_body = body.clone();
        let request_state = Arc::clone(&state);
        let request = std::thread::spawn(move || {
            handle_runtime_request(
                "POST",
                "/api/deployments/example.live/intents",
                Some(&body),
                &request_state,
            )
        });

        entered_rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("venue submit entered");
        let daemon_guard = state
            .daemon
            .try_lock()
            .expect("daemon mutex must be available during venue submit");
        assert_eq!(
            daemon_guard
                .trading
                .get("example.live")
                .and_then(|runtime| runtime.order("order-request-blocking-1"))
                .expect("pending order")
                .state,
            ploy_trading::OrderState::Pending
        );
        drop(daemon_guard);

        let retry_state = Arc::clone(&state);
        let (retry_tx, retry_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = retry_tx.send(handle_runtime_request(
                "POST",
                "/api/deployments/example.live/intents",
                Some(&retry_body),
                &retry_state,
            ));
        });
        let (retry_status, retry_response) = retry_rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("idempotent retry must not wait for venue");
        assert_eq!(retry_status, 200);
        assert!(retry_response.contains("\"state\":\"pending\""));
        assert_eq!(submits.load(Ordering::SeqCst), 1);

        release_tx.send(()).expect("release submit");
        let (status, response) = request.join().expect("request thread");
        assert_eq!(status, 200);
        assert!(response.contains("\"state\":\"acknowledged\""));
    }

    #[test]
    fn handle_runtime_request_surfaces_live_gateway_transport_failure_as_503() {
        let root = temp_dir("runtime-live-intent-transport-error");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": "0x1111111111111111111111111111111111111111",
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root,
            status_file: root.join("run/platform/system-status.json"),
            deployment_status_file: root.join("run/platform/deployments.json"),
            trading_state_file: root.join("run/platform/trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        crate::runtime::seed_empty_live_ledgers(&config);
        let mut daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::failed(
                ploy_connectivity::ExecutionError::Transport("gateway offline".to_string()),
            )),
        )
        .expect("boot daemon");
        daemon.control_plane.deployments.set_observed_state(
            "example.live",
            ploy_operator_contracts::ObservedState::Running,
        );
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let body = serde_json::to_string(&PaperIntentRequest {
            idempotency_key: None,
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::new(5, 1)),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");

        let (submit_code, submit_response) = handle_runtime_request(
            "POST",
            "/api/deployments/example.live/intents",
            Some(&body),
            &state,
        );
        assert_eq!(submit_code, 200);
        assert!(submit_response.contains("\"state\":\"unknown\""));
        assert!(submit_response.contains("gateway offline"));

        let trading_body =
            fs::read_to_string(root.join("run/platform/trading-state.json")).expect("snapshot");
        let trading: serde_json::Value =
            serde_json::from_str(&trading_body).expect("snapshot json");
        assert_eq!(trading[0]["orders"][0]["state"], "unknown");
        assert_eq!(trading[0]["deployment_id"], "example.live");
    }

    #[test]
    fn handle_runtime_request_cancels_live_order_and_persists_snapshot() {
        let root = temp_dir("runtime-live-cancel");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": "0x1111111111111111111111111111111111111111",
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let gateway = StaticExecutionGateway::acknowledged("venue-live-http-cancel-1")
            .with_cancel_result(Ok(CancellationOutcome::Canceled));
        crate::runtime::seed_empty_live_ledgers(&config);
        let mut daemon =
            crate::runtime::PloyDaemon::boot_with_live_execution(&config, Box::new(gateway))
                .expect("boot daemon");
        daemon.control_plane.deployments.set_observed_state(
            "example.live",
            ploy_operator_contracts::ObservedState::Running,
        );
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let submit_body = serde_json::to_string(&PaperIntentRequest {
            idempotency_key: None,
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::new(5, 1)),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");

        let (submit_code, submit_response) = handle_runtime_request(
            "POST",
            "/api/deployments/example.live/intents",
            Some(&submit_body),
            &state,
        );
        assert_eq!(submit_code, 200);
        assert!(submit_response.contains("\"order_id\":\"order-"));

        let order_id = submit_response
            .split("\"order_id\":\"")
            .nth(1)
            .and_then(|suffix| suffix.split('"').next())
            .expect("order id");

        let (cancel_code, cancel_response) = handle_runtime_request(
            "POST",
            &format!("/api/deployments/example.live/orders/{order_id}/cancel"),
            None,
            &state,
        );
        assert_eq!(cancel_code, 200);
        assert!(cancel_response.contains("\"state\":\"canceled\""));

        let trading_body =
            fs::read_to_string(runtime_root.join("trading-state.json")).expect("trading snapshot");
        let trading: serde_json::Value =
            serde_json::from_str(&trading_body).expect("snapshot json");
        assert_eq!(trading[0]["orders"][0]["state"], "canceled");
        assert_eq!(
            trading[0]["orders"][0]["venue_order_id"],
            "venue-live-http-cancel-1"
        );
    }

    #[test]
    fn handle_runtime_request_replaces_live_order_and_persists_revision_history() {
        let root = temp_dir("runtime-live-replace");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": "0x1111111111111111111111111111111111111111",
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let gateway = StaticExecutionGateway::acknowledged("venue-live-http-replace-1")
            .with_replace_result(Ok(ReplaceOutcome::Replaced {
                venue_order_id: "venue-live-http-replace-2".to_string(),
            }));
        crate::runtime::seed_empty_live_ledgers(&config);
        let mut daemon =
            crate::runtime::PloyDaemon::boot_with_live_execution(&config, Box::new(gateway))
                .expect("boot daemon");
        daemon.control_plane.deployments.set_observed_state(
            "example.live",
            ploy_operator_contracts::ObservedState::Running,
        );
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let submit_body = serde_json::to_string(&PaperIntentRequest {
            idempotency_key: None,
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::new(55, 2)),
            purpose: ploy_operator_contracts::IntentPurpose::Entry,
        })
        .expect("request json");

        let (submit_code, submit_response) = handle_runtime_request(
            "POST",
            "/api/deployments/example.live/intents",
            Some(&submit_body),
            &state,
        );
        assert_eq!(submit_code, 200);

        let order_id = submit_response
            .split("\"order_id\":\"")
            .nth(1)
            .and_then(|suffix| suffix.split('"').next())
            .expect("order id");

        let replace_body = serde_json::to_string(&OrderReplaceRequest {
            quantity: rust_decimal::Decimal::new(250, 2),
            limit_price: Some(rust_decimal::Decimal::new(57, 2)),
        })
        .expect("replace body");
        let (replace_code, replace_response) = handle_runtime_request(
            "POST",
            &format!("/api/deployments/example.live/orders/{order_id}/replace"),
            Some(&replace_body),
            &state,
        );
        assert_eq!(replace_code, 200);
        assert!(replace_response.contains("\"revision\":1"));
        assert!(replace_response.contains("\"venue_order_id\":\"venue-live-http-replace-2\""));

        let trading_body =
            fs::read_to_string(runtime_root.join("trading-state.json")).expect("trading snapshot");
        let trading: serde_json::Value =
            serde_json::from_str(&trading_body).expect("snapshot json");
        assert_eq!(
            trading[0]["orders"][0]["venue_order_history"][0],
            "venue-live-http-replace-1"
        );
        assert_eq!(trading[0]["orders"][0]["revision"], 1);
    }

    #[test]
    fn handle_runtime_request_reads_trading_state_from_shared_daemon() {
        let root = temp_dir("runtime-live-read");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.live",
                    "bundle_id": "example",
                    "runtime_mode": "live",
                    "account_id": "0x1111111111111111111111111111111111111111",
                    "max_gross_exposure": "5",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root,
            status_file: root.join("run/platform/system-status.json"),
            deployment_status_file: root.join("run/platform/deployments.json"),
            trading_state_file: root.join("run/platform/trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        crate::runtime::seed_empty_live_ledgers(&config);
        let mut daemon = crate::runtime::PloyDaemon::boot_with_live_execution(
            &config,
            Box::new(StaticExecutionGateway::acknowledged("venue-live-http-2")),
        )
        .expect("boot daemon");
        daemon.control_plane.deployments.set_observed_state(
            "example.live",
            ploy_operator_contracts::ObservedState::Running,
        );
        daemon
            .submit_intent(TradingIntent {
                intent_id: "intent-live-http-2".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: rust_decimal::Decimal::ONE,
                limit_price: Some(rust_decimal::Decimal::new(5, 1)),
                purpose: TradingIntentPurpose::Entry,
                created_at: chrono::Utc::now(),
            })
            .expect("submit intent");

        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) = handle_runtime_request("GET", "/api/trading/state", None, &state);
        assert_eq!(status_code, 200);
        assert!(body.contains("\"deployment_id\":\"example.live\""));
        assert!(body.contains("\"venue_order_id\":\"venue-live-http-2\""));
    }

    #[test]
    fn handle_runtime_request_reports_structured_not_found_error() {
        let daemon = crate::runtime::PloyDaemon::boot(&crate::config::PlatformConfig::default())
            .expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) =
            handle_runtime_request("GET", "/api/deployments/missing.paper", None, &state);
        assert_eq!(status_code, 404);
        assert!(body.contains("\"error\":\"deployment_not_found\""));
        assert!(body.contains("\"message\""));
        assert!(body.contains("missing.paper"));
    }

    #[test]
    fn handle_runtime_request_reports_poisoned_lock_as_503() {
        let daemon = crate::runtime::PloyDaemon::boot(&crate::config::PlatformConfig::default())
            .expect("boot daemon");
        let poisoned = Arc::new(Mutex::new(daemon));
        let poison_handle = poisoned.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poison_handle.lock().expect("lock daemon");
            panic!("poison daemon lock for test");
        })
        .join();

        let state = Arc::new(AppState {
            daemon: poisoned,
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) =
            handle_runtime_request("GET", "/api/deployments/missing.paper", None, &state);
        assert_eq!(status_code, 503);
        assert!(body.contains("\"error\":\"daemon_lock_poisoned\""));
    }

    #[test]
    fn handle_runtime_request_reads_single_agent_run_detail() {
        let root = temp_dir("runtime-agent-run-detail");
        let runtime_root = root.join("run/platform");
        let agent_runs_file = root.join("run/sidecar/agent-runs.jsonl");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(agent_runs_file.parent().expect("agent runs parent"))
            .expect("create agent runs dir");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(&registry_file, "[]").expect("registry");
        fs::write(
            &agent_runs_file,
            serde_json::json!({
                "run_id": "run-operator-detail",
                "cycle_kind": "oversight",
                "status": "succeeded",
                "started_at": Utc::now(),
                "finished_at": Utc::now(),
                "session_id": "session-1",
                "model": "claude-sonnet-4.5",
                "platform_status": "running",
                "deployment_count": 1,
                "oversight_signal_count": 2,
                "oversight_playbook_count": 1,
                "total_cost_usd": 0.12,
                "tool_calls": [
                    { "name": "mcp__research__check_oversight", "status": "succeeded" }
                ],
                "research_reports": 1,
                "oversight_alerts": 1,
                "operator_recommendations": 1,
                "failure_reason": null,
                "runtime_context": {
                    "deployment_sample": ["example.paper paper example"],
                    "oversight_signal_summary": ["critical order_buildup"],
                    "oversight_playbook_summary": ["replay example.paper"],
                    "diagnostic_candidates": ["example.paper"]
                },
                "output_summary": {
                    "research_report_summaries": ["replay example.paper filled 12 events"],
                    "oversight_alert_summaries": ["critical order_buildup"],
                    "operator_recommendation_summaries": ["pause review example.paper"]
                },
                "evaluation": {
                    "usefulness": "high",
                    "research_reports": 1,
                    "oversight_alerts": 1,
                    "operator_recommendations": 1
                }
            })
            .to_string(),
        )
        .expect("agent runs");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            agent_runs_file,
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) =
            handle_runtime_request("GET", "/api/agent/runs/run-operator-detail", None, &state);
        assert_eq!(status_code, 200);
        assert!(body.contains("\"run_id\":\"run-operator-detail\""));
        assert!(body.contains("\"diagnostic_candidates\":[\"example.paper\"]"));
    }

    #[test]
    fn handle_runtime_request_reads_nullable_sidecar_agent_runs() {
        let root = temp_dir("runtime-agent-run-nullable");
        let runtime_root = root.join("run/platform");
        let agent_runs_file = root.join("run/sidecar/agent-runs.jsonl");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(agent_runs_file.parent().expect("agent runs parent"))
            .expect("create agent runs dir");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(&registry_file, "[]").expect("registry");
        let valid_run = serde_json::json!({
            "run_id": "run-nullable",
            "cycle_kind": "research_oversight",
            "status": "started",
            "started_at": Utc::now(),
            "finished_at": null,
            "session_id": null,
            "model": "sonnet",
            "platform_status": null,
            "deployment_count": 0,
            "oversight_signal_count": 0,
            "oversight_playbook_count": 0,
            "total_cost_usd": null,
            "tool_calls": [],
            "research_reports": 0,
            "oversight_alerts": 0,
            "operator_recommendations": 0,
            "failure_reason": null,
            "runtime_context": null,
            "output_summary": null,
            "evaluation": null
        })
        .to_string();
        fs::write(&agent_runs_file, format!("{{broken json\n{valid_run}\n")).expect("agent runs");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            agent_runs_file,
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) = handle_runtime_request("GET", "/api/agent/runs", None, &state);
        assert_eq!(status_code, 200);
        assert!(body.contains("\"run_id\":\"run-nullable\""));
        assert!(body.contains("\"finished_at\":null"));
    }

    #[test]
    fn handle_runtime_request_reads_harness_memory() {
        let root = temp_dir("runtime-harness-memory");
        let runtime_root = root.join("run/platform");
        let agent_runs_file = root.join("run/sidecar/agent-runs.jsonl");
        let context_file = root.join("run/sidecar/harness-context.md");
        let events_file = root.join("run/sidecar/harness-events.jsonl");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(agent_runs_file.parent().expect("agent runs parent"))
            .expect("create agent runs dir");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(&registry_file, "[]").expect("registry");
        fs::write(&agent_runs_file, "").expect("agent runs");
        fs::write(&context_file, "# Harness\n\n- learned: use grok-evidence\n").expect("context");
        fs::write(
            &events_file,
            format!(
                "{{bad json\n{}\n",
                serde_json::json!({
                    "kind": "harness_learning",
                    "run_id": "agent-test",
                    "category": "tool_gap",
                    "summary": "missing WebSearch"
                })
            ),
        )
        .expect("events");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            agent_runs_file,
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let (status_code, body) =
            handle_runtime_request("GET", "/api/agent/harness-memory", None, &state);
        assert_eq!(status_code, 200);
        assert!(body.contains("use grok-evidence"));
        assert!(body.contains("\"event_count\":1"));
        assert!(body.contains("\"summary\":\"missing WebSearch\""));
        assert!(!body.contains("harness-context.md"));
    }

    #[test]
    fn handle_runtime_request_queues_agent_run_request() {
        let root = temp_dir("runtime-agent-run-queue");
        let runtime_root = root.join("run/platform");
        let agent_runs_file = root.join("run/sidecar/agent-runs.jsonl");
        let request_file = root.join("run/sidecar/agent-run-requests.jsonl");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(agent_runs_file.parent().expect("agent runs parent"))
            .expect("create agent runs dir");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(&registry_file, "[]").expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            agent_runs_file: agent_runs_file.clone(),
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let request = serde_json::json!({
            "objective": "Find a gated PM5D research candidate",
            "strategy_profile": "pm5d.settlement_probability.agent",
            "autonomy_mode": "research_until_blocked",
            "target_evidence": "executable_replay",
            "symbols": ["BTCUSDT", "ETHUSDT"],
            "max_turns": 30,
            "budget_usd": 1.0,
            "run_packet": "# packet",
            "run_contract": "[agentic_strategy_run]\ncompletion_signal = \"required\""
        })
        .to_string();

        let (status_code, body) =
            handle_runtime_request("POST", "/api/agent/runs", Some(&request), &state);
        assert_eq!(status_code, 202);
        let response: ploy_operator_contracts::AgentRunCreateResponse =
            serde_json::from_str(&body).expect("queue response");
        assert_eq!(response.status, "requested");
        assert!(response.run_id.starts_with("agent-"));
        assert!(!body.contains("request_path"));
        assert!(request_file.exists());
        assert!(agent_runs_file.exists());

        let queued_request = fs::read_to_string(request_file).expect("request file");
        assert!(queued_request.contains("\"objective\":\"Find a gated PM5D research candidate\""));
        let queued_runs = fs::read_to_string(agent_runs_file).expect("runs file");
        assert!(queued_runs.contains("\"status\":\"requested\""));

        let over_limit = serde_json::json!({
            "objective":"bounded", "strategy_profile":"test", "autonomy_mode":"research_until_blocked",
            "target_evidence":"diagnostic", "symbols":["BTCUSDT"], "max_turns":31,
            "budget_usd":1.0, "run_packet":"packet", "run_contract":"contract"
        }).to_string();
        let (status_code, body) =
            handle_runtime_request("POST", "/api/agent/runs", Some(&over_limit), &state);
        assert_eq!(status_code, 400);
        assert!(body.contains("agent_run_limits_exceeded"));

        for (max_turns, budget_usd) in [
            (0, serde_json::json!(1.0)),
            (1, serde_json::json!(0.0)),
            (1, serde_json::json!(1.01)),
        ] {
            let invalid = serde_json::json!({
                "objective":"bounded", "strategy_profile":"test",
                "autonomy_mode":"research_until_blocked", "target_evidence":"diagnostic",
                "symbols":["BTCUSDT"], "max_turns":max_turns, "budget_usd":budget_usd,
                "run_packet":"packet", "run_contract":"contract"
            })
            .to_string();
            let (status_code, body) =
                handle_runtime_request("POST", "/api/agent/runs", Some(&invalid), &state);
            assert_eq!(status_code, 400);
            assert!(body.contains("agent_run_limits_exceeded"));
        }

        for invalid_json in [
            r#"{"objective":"bounded","strategy_profile":"test","autonomy_mode":"research_until_blocked","target_evidence":"diagnostic","symbols":[],"budget_usd":1.0,"run_packet":"packet","run_contract":"contract"}"#,
            r#"{"objective":"bounded","strategy_profile":"test","autonomy_mode":"research_until_blocked","target_evidence":"diagnostic","symbols":[],"max_turns":1,"run_packet":"packet","run_contract":"contract"}"#,
            r#"{"objective":"bounded","strategy_profile":"test","autonomy_mode":"research_until_blocked","target_evidence":"diagnostic","symbols":[],"max_turns":1,"budget_usd":NaN,"run_packet":"packet","run_contract":"contract"}"#,
            r#"{"objective":"bounded","strategy_profile":"test","autonomy_mode":"research_until_blocked","target_evidence":"diagnostic","symbols":[],"max_turns":1,"budget_usd":Infinity,"run_packet":"packet","run_contract":"contract"}"#,
        ] {
            let (status_code, body) =
                handle_runtime_request("POST", "/api/agent/runs", Some(invalid_json), &state);
            assert_eq!(status_code, 400);
            assert!(body.contains("invalid_json"));
        }
    }

    #[test]
    fn append_jsonl_normalizes_a_complete_tail_without_newline() {
        let root = temp_dir("jsonl-complete-tail");
        let path = root.join("events.jsonl");
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(&path, br#"{"first":1}"#).expect("write complete tail");

        append_jsonl(&path, &serde_json::json!({"second": 2})).expect("append record");

        let body = fs::read_to_string(&path).expect("read JSONL");
        assert_eq!(body.lines().count(), 2);
        for line in body.lines() {
            serde_json::from_str::<serde_json::Value>(line).expect("valid line");
        }
    }

    #[test]
    fn append_jsonl_refuses_to_join_onto_a_truncated_tail() {
        let root = temp_dir("jsonl-truncated-tail");
        let path = root.join("events.jsonl");
        fs::create_dir_all(&root).expect("create temp root");
        fs::write(&path, br#"{"first":"truncated"#).expect("write truncated tail");

        let error = append_jsonl(&path, &serde_json::json!({"second": 2}))
            .expect_err("truncated tail must fail closed");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("truncated JSONL tail"));
        assert_eq!(
            fs::read(&path).expect("preserved tail"),
            br#"{"first":"truncated"#
        );
    }

    #[test]
    fn handle_runtime_request_creates_and_lists_proposals() {
        let root = temp_dir("runtime-proposals-create");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "account_id": "paper:test-proposal",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let create_body = serde_json::to_string(&ploy_operator_contracts::ProposalCreateRequest {
            action_kind: ploy_operator_contracts::ProposalActionKind::PauseDeployment,
            target_deployment_id: "example.paper".to_string(),
            rationale: "pnl regression crossed threshold".to_string(),
            evidence: vec!["net_pnl=-2.50".to_string()],
            source_run_id: Some("run-123".to_string()),
            proposed_max_gross_exposure: None,
        })
        .expect("proposal create json");

        let (create_code, create_response) =
            handle_runtime_request("POST", "/api/proposals", Some(&create_body), &state);
        assert_eq!(create_code, 200);
        assert!(create_response.contains("\"proposal_id\""));
        assert!(create_response.contains("\"action_kind\":\"pause_deployment\""));

        let (list_code, list_response) =
            handle_runtime_request("GET", "/api/proposals", None, &state);
        assert_eq!(list_code, 200);
        assert!(list_response.contains("\"proposal_id\""));
        assert!(list_response.contains("\"status\":\"pending\""));
    }

    #[test]
    fn handle_runtime_request_reads_single_proposal_detail() {
        let root = temp_dir("runtime-proposals-detail");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let create_body = serde_json::to_string(&ploy_operator_contracts::ProposalCreateRequest {
            action_kind: ploy_operator_contracts::ProposalActionKind::PauseDeployment,
            target_deployment_id: "example.paper".to_string(),
            rationale: "drift crossed threshold".to_string(),
            evidence: vec!["drawdown=3.20".to_string()],
            source_run_id: Some("run-detail-1".to_string()),
            proposed_max_gross_exposure: None,
        })
        .expect("proposal create json");

        let (_, create_response) =
            handle_runtime_request("POST", "/api/proposals", Some(&create_body), &state);
        let proposal =
            serde_json::from_str::<ploy_operator_contracts::SafetyProposal>(&create_response)
                .expect("proposal json");

        let (status_code, body) = handle_runtime_request(
            "GET",
            &format!("/api/proposals/{}", proposal.proposal_id),
            None,
            &state,
        );
        assert_eq!(status_code, 200);
        assert!(body.contains(&proposal.proposal_id));
        assert!(body.contains("\"source_run_id\":\"run-detail-1\""));
    }

    #[test]
    fn handle_runtime_request_approves_pause_proposal_through_control_plane() {
        let root = temp_dir("runtime-proposals-approve");
        let runtime_root = root.join("run/platform");
        let registry_file = root.join("data/state/deployments.json");
        fs::create_dir_all(runtime_root.clone()).expect("create runtime root");
        fs::create_dir_all(registry_file.parent().expect("registry parent")).expect("create");
        fs::write(
            &registry_file,
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "bundle_id": "example",
                    "runtime_mode": "paper",
                    "account_id": "paper:test-proposal-approval",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("registry");

        let config = crate::config::PlatformConfig {
            registry_file,
            runtime_root: runtime_root.clone(),
            status_file: runtime_root.join("system-status.json"),
            deployment_status_file: runtime_root.join("deployments.json"),
            trading_state_file: runtime_root.join("trading-state.json"),
            ..crate::config::PlatformConfig::default()
        };

        let daemon = crate::runtime::PloyDaemon::boot(&config).expect("boot daemon");
        let state = Arc::new(AppState {
            daemon: Arc::new(Mutex::new(daemon)),
            events: Arc::new(EventBroker::default()),
        });

        let create_body = serde_json::to_string(&ploy_operator_contracts::ProposalCreateRequest {
            action_kind: ploy_operator_contracts::ProposalActionKind::PauseDeployment,
            target_deployment_id: "example.paper".to_string(),
            rationale: "orders accumulated too quickly".to_string(),
            evidence: vec!["active_orders=8".to_string()],
            source_run_id: Some("run-456".to_string()),
            proposed_max_gross_exposure: None,
        })
        .expect("proposal create json");

        let (_, create_response) =
            handle_runtime_request("POST", "/api/proposals", Some(&create_body), &state);
        let proposal_id =
            serde_json::from_str::<ploy_operator_contracts::SafetyProposal>(&create_response)
                .expect("proposal json")
                .proposal_id;

        let approve_body =
            serde_json::to_string(&ploy_operator_contracts::ProposalDecisionRequest {
                decision_note: Some("approved in test".to_string()),
            })
            .expect("approve json");
        let (approve_code, approve_response) = handle_runtime_request(
            "POST",
            &format!("/api/proposals/{proposal_id}/approve"),
            Some(&approve_body),
            &state,
        );
        assert_eq!(approve_code, 200);
        assert!(approve_response.contains("\"status\":\"approved\""));

        let (deployment_code, deployment_response) =
            handle_runtime_request("GET", "/api/deployments/example.paper", None, &state);
        assert_eq!(deployment_code, 200);
        assert!(deployment_response.contains("\"desired_state\":\"paused\""));
    }
}
