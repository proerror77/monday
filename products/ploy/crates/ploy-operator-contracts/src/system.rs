use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatState {
    Healthy,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HeartbeatStatus {
    pub source_id: String,
    pub source_kind: String,
    pub state: HeartbeatState,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub stale_after_seconds: i64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    SourceStale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActiveAlert {
    pub alert_id: String,
    pub kind: AlertKind,
    pub severity: AlertSeverity,
    pub source_id: String,
    pub message: String,
    pub triggered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PlatformMetrics {
    pub total_deployments: usize,
    pub live_deployments: usize,
    pub degraded_deployments: usize,
    pub active_alerts: usize,
    pub stale_sources: usize,
    pub live_reconcile_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_cpu_pressure_milli_percent: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_load_average_1m_milli: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_memory_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_memory_available_mb: Option<u64>,
    pub last_trade_time: Option<DateTime<Utc>>,
    pub last_live_reconcile_success_at: Option<DateTime<Utc>>,
    pub heartbeats: Vec<HeartbeatStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SystemStatus {
    pub status: String,
    pub uptime_seconds: i64,
    pub version: String,
    pub strategy: String,
    pub last_trade_time: Option<DateTime<Utc>>,
    pub websocket_connected: bool,
    pub database_connected: bool,
    pub error_count_1h: i64,
    #[serde(default)]
    pub live_reconcile_failures: u32,
    #[serde(default)]
    pub next_live_reconcile_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_live_reconcile_error: Option<String>,
    #[serde(default)]
    pub active_alert_count: usize,
    #[serde(default)]
    pub stale_source_count: usize,
    #[serde(default)]
    pub last_live_reconcile_success_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SystemControlResponse {
    pub success: bool,
    pub message: String,
}
