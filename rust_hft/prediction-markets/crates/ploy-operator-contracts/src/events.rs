use crate::deployments::DeploymentSummary;
use crate::diagnostics::{OversightSnapshotEvent, ProposalSnapshotEvent};
use crate::system::{ActiveAlert, PlatformMetrics, SystemStatus};
use crate::trading::{MarketData, PositionResponse, TradeResponse, TradingStateSnapshot};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub component: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusUpdate {
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SystemSnapshotEvent {
    pub system: SystemStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeploymentSnapshotEvent {
    pub deployments: Vec<DeploymentSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TradingSnapshotEvent {
    pub trading: Vec<TradingStateSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MetricsSnapshotEvent {
    pub metrics: PlatformMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AlertSnapshotEvent {
    pub alerts: Vec<ActiveAlert>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "data")]
pub enum OperatorEvent {
    #[serde(rename = "log")]
    Log(LogEntry),
    #[serde(rename = "trade")]
    Trade(TradeResponse),
    #[serde(rename = "position")]
    Position(PositionResponse),
    #[serde(rename = "market")]
    Market(MarketData),
    #[serde(rename = "status")]
    Status(StatusUpdate),
    #[serde(rename = "system_snapshot")]
    SystemSnapshot(SystemSnapshotEvent),
    #[serde(rename = "deployment_snapshot")]
    DeploymentSnapshot(DeploymentSnapshotEvent),
    #[serde(rename = "trading_snapshot")]
    TradingSnapshot(TradingSnapshotEvent),
    #[serde(rename = "metrics_snapshot")]
    MetricsSnapshot(MetricsSnapshotEvent),
    #[serde(rename = "alert_snapshot")]
    AlertSnapshot(AlertSnapshotEvent),
    #[serde(rename = "oversight_snapshot")]
    OversightSnapshot(OversightSnapshotEvent),
    #[serde(rename = "proposal_snapshot")]
    ProposalSnapshot(ProposalSnapshotEvent),
}

pub type WsMessage = OperatorEvent;

#[cfg(test)]
mod tests {
    use super::{
        AlertSnapshotEvent, DeploymentSnapshotEvent, MetricsSnapshotEvent, OperatorEvent,
        StatusUpdate, SystemSnapshotEvent, TradingSnapshotEvent,
    };
    use crate::{
        ActiveAlert, AlertKind, AlertSeverity, DeploymentState, DeploymentSummary, DesiredState,
        HeartbeatState, HeartbeatStatus, ObservedState, PlatformMetrics, SystemStatus,
        TradingStateSnapshot,
    };
    use chrono::Utc;
    use rust_decimal::Decimal;
    use serde_json::json;

    #[test]
    fn websocket_event_kind_serializes_as_status() {
        let value = serde_json::to_value(OperatorEvent::Status(StatusUpdate {
            status: "running".to_string(),
        }))
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "type": "status",
                "data": {
                    "status": "running",
                }
            })
        );
    }

    #[test]
    fn deployment_snapshot_event_uses_stable_wire_shape() {
        let value =
            serde_json::to_value(OperatorEvent::DeploymentSnapshot(DeploymentSnapshotEvent {
                deployments: vec![DeploymentSummary {
                    deployment_id: "example.paper".to_string(),
                    runtime_mode: crate::deployments::DeploymentRuntimeMode::Paper,
                    account_id: "acct-paper".to_string(),
                    max_gross_exposure: Some(Decimal::new(500, 2)),
                    deployment_state: DeploymentState::Enabled,
                    desired_state: DesiredState::Running,
                    observed_state: ObservedState::Running,
                }],
            }))
            .expect("to_value");

        assert_eq!(
            value,
            json!({
                "type": "deployment_snapshot",
                "data": {
                    "deployments": [{
                        "deployment_id": "example.paper",
                        "runtime_mode": "paper",
                        "account_id": "acct-paper",
                        "max_gross_exposure": "5.00",
                        "deployment_state": "enabled",
                        "desired_state": "running",
                        "observed_state": "running",
                    }]
                }
            })
        );
    }

    #[test]
    fn system_snapshot_event_uses_stable_wire_shape() {
        let value = serde_json::to_value(OperatorEvent::SystemSnapshot(SystemSnapshotEvent {
            system: SystemStatus {
                status: "running".to_string(),
                uptime_seconds: 1,
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
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "type": "system_snapshot",
                "data": {
                    "system": {
                        "status": "running",
                        "uptime_seconds": 1,
                        "version": "0.1.0",
                        "strategy": "platform",
                        "last_trade_time": null,
                        "websocket_connected": false,
                        "database_connected": false,
                        "error_count_1h": 0,
                        "live_reconcile_failures": 0,
                        "next_live_reconcile_at": null,
                        "last_live_reconcile_error": null,
                        "active_alert_count": 0,
                        "stale_source_count": 0,
                        "last_live_reconcile_success_at": null,
                    }
                }
            })
        );
    }

    #[test]
    fn trading_snapshot_event_uses_stable_wire_shape() {
        let value = serde_json::to_value(OperatorEvent::TradingSnapshot(TradingSnapshotEvent {
            trading: vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: crate::DeploymentRuntimeMode::Paper,
                ..TradingStateSnapshot::default()
            }],
        }))
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "type": "trading_snapshot",
                "data": {
                    "trading": [{
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
                            "net_pnl": "0",
                        },
                        "risk": {
                            "pending_intents": 0,
                            "active_orders": 0,
                            "open_positions": 0,
                            "gross_exposure": "0",
                            "reserved_order_exposure": "0",
                            "total_gross_exposure": "0",
                        },
                    }]
                }
            })
        );
    }

    #[test]
    fn metrics_snapshot_event_uses_stable_wire_shape() {
        let value = serde_json::to_value(OperatorEvent::MetricsSnapshot(MetricsSnapshotEvent {
            metrics: PlatformMetrics {
                total_deployments: 2,
                live_deployments: 1,
                degraded_deployments: 1,
                active_alerts: 1,
                stale_sources: 1,
                live_reconcile_failures: 2,
                host_cpu_pressure_milli_percent: Some(42_500),
                host_load_average_1m_milli: Some(850),
                process_memory_mb: Some(128),
                host_memory_available_mb: Some(1024),
                last_trade_time: None,
                last_live_reconcile_success_at: None,
                heartbeats: vec![HeartbeatStatus {
                    source_id: "venue:polymarket".to_string(),
                    source_kind: "venue".to_string(),
                    state: HeartbeatState::Stale,
                    last_seen_at: Some(Utc::now()),
                    stale_after_seconds: 15,
                    message: Some("gateway offline".to_string()),
                }],
            },
        }))
        .expect("to_value");

        assert_eq!(value["type"], json!("metrics_snapshot"));
        assert_eq!(value["data"]["metrics"]["total_deployments"], json!(2));
        assert_eq!(
            value["data"]["metrics"]["heartbeats"][0]["state"],
            json!("stale")
        );
    }

    #[test]
    fn alert_snapshot_event_uses_stable_wire_shape() {
        let value = serde_json::to_value(OperatorEvent::AlertSnapshot(AlertSnapshotEvent {
            alerts: vec![ActiveAlert {
                alert_id: "venue:polymarket:stale".to_string(),
                kind: AlertKind::SourceStale,
                severity: AlertSeverity::Critical,
                source_id: "venue:polymarket".to_string(),
                message: "gateway offline".to_string(),
                triggered_at: Utc::now(),
            }],
        }))
        .expect("to_value");

        assert_eq!(value["type"], json!("alert_snapshot"));
        assert_eq!(value["data"]["alerts"][0]["kind"], json!("source_stale"));
        assert_eq!(value["data"]["alerts"][0]["severity"], json!("critical"));
    }
}
