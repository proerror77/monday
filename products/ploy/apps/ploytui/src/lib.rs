use ploy_operator_contracts::{
    ActiveAlert, DeploymentSummary, OperatorEvent, PlatformMetrics, SystemStatus,
    TradingStateSnapshot,
};
use std::fmt::Write;

#[derive(Debug, Clone)]
pub struct DashboardSnapshot {
    pub system: SystemStatus,
    pub metrics: PlatformMetrics,
    pub alerts: Vec<ActiveAlert>,
    pub deployments: Vec<DeploymentSummary>,
    pub trading: Vec<TradingStateSnapshot>,
    pub recent_events: Vec<OperatorEvent>,
}

pub fn render_dashboard(snapshot: &DashboardSnapshot) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "PLOY TUI");
    let _ = writeln!(out, "========");
    let _ = writeln!(out);

    let _ = writeln!(out, "System");
    let _ = writeln!(
        out,
        "status={} uptime={}s version={} errors_1h={} active_alerts={} stale_sources={}",
        snapshot.system.status,
        snapshot.system.uptime_seconds,
        snapshot.system.version,
        snapshot.system.error_count_1h,
        snapshot.system.active_alert_count,
        snapshot.system.stale_source_count,
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "Metrics");
    let _ = writeln!(
        out,
        "deployments_total={} live={} degraded={} live_reconcile_failures={} last_live_reconcile_success_at={}",
        snapshot.metrics.total_deployments,
        snapshot.metrics.live_deployments,
        snapshot.metrics.degraded_deployments,
        snapshot.metrics.live_reconcile_failures,
        snapshot
            .metrics
            .last_live_reconcile_success_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );
    if snapshot.metrics.heartbeats.is_empty() {
        let _ = writeln!(out, "  heartbeats=none");
    } else {
        for heartbeat in &snapshot.metrics.heartbeats {
            let _ = writeln!(
                out,
                "  {} {} state={} last_seen={} message={}",
                heartbeat.source_kind,
                heartbeat.source_id,
                format!("{:?}", heartbeat.state).to_lowercase(),
                heartbeat
                    .last_seen_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
                heartbeat.message.clone().unwrap_or_else(|| "-".to_string())
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Active Alerts");
    if snapshot.alerts.is_empty() {
        let _ = writeln!(out, "  none");
    } else {
        for alert in &snapshot.alerts {
            let _ = writeln!(
                out,
                "  {} {} {} {}",
                alert.triggered_at.to_rfc3339(),
                format!("{:?}", alert.severity).to_lowercase(),
                alert.source_id,
                alert.message
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Deployments");
    if snapshot.deployments.is_empty() {
        let _ = writeln!(out, "  none");
    } else {
        for deployment in &snapshot.deployments {
            let _ = writeln!(
                out,
                "  {} lifecycle={} desired={} observed={}",
                deployment.deployment_id,
                state_name(&deployment.deployment_state),
                state_name(&deployment.desired_state),
                state_name(&deployment.observed_state)
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Trading");
    if snapshot.trading.is_empty() {
        let _ = writeln!(out, "  none");
    } else {
        for trading in &snapshot.trading {
            let _ = writeln!(
                out,
                "  {} mode={} intents={} orders={} fills={} positions={} net_pnl={}",
                trading.deployment_id,
                trading.runtime_mode,
                trading.intents.len(),
                trading.orders.len(),
                trading.fills.len(),
                trading.positions.len(),
                trading.pnl.net_pnl
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Recent Events");
    if snapshot.recent_events.is_empty() {
        let _ = writeln!(out, "  none");
    } else {
        for event in &snapshot.recent_events {
            let _ = writeln!(out, "  {}", render_event_line(event));
        }
    }

    out
}

pub fn render_event_line(event: &OperatorEvent) -> String {
    match event {
        OperatorEvent::SystemSnapshot(event) => format!(
            "system_snapshot status={} uptime={}s",
            event.system.status, event.system.uptime_seconds
        ),
        OperatorEvent::DeploymentSnapshot(event) => {
            format!("deployment_snapshot count={}", event.deployments.len())
        }
        OperatorEvent::TradingSnapshot(event) => {
            format!("trading_snapshot count={}", event.trading.len())
        }
        OperatorEvent::MetricsSnapshot(event) => format!(
            "metrics_snapshot alerts={} stale_sources={}",
            event.metrics.active_alerts, event.metrics.stale_sources
        ),
        OperatorEvent::AlertSnapshot(event) => {
            format!("alert_snapshot count={}", event.alerts.len())
        }
        OperatorEvent::OversightSnapshot(event) => format!(
            "oversight_snapshot status={} signals={}",
            event.oversight.platform_status, event.oversight.signal_count
        ),
        OperatorEvent::ProposalSnapshot(event) => {
            format!("proposal_snapshot count={}", event.proposals.len())
        }
        OperatorEvent::Status(event) => format!("status {}", event.status),
        OperatorEvent::Log(event) => format!("log {} {}", event.level, event.message),
        OperatorEvent::Trade(event) => format!(
            "trade {} {} shares={} status={}",
            event.id, event.token_id, event.shares, event.status
        ),
        OperatorEvent::Position(event) => format!(
            "position {} {} shares={}",
            event.token_id, event.side, event.shares
        ),
        OperatorEvent::Market(event) => format!(
            "market {} bid={} ask={}",
            event.token_id, event.best_bid, event.best_ask
        ),
    }
}

fn state_name<T: std::fmt::Debug>(value: &T) -> String {
    format!("{value:?}").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{render_dashboard, render_event_line, DashboardSnapshot};
    use chrono::Utc;
    use ploy_operator_contracts::{
        ActiveAlert, AlertKind, AlertSeverity, DeploymentSnapshotEvent, DeploymentState,
        DeploymentSummary, DesiredState, HeartbeatState, HeartbeatStatus, ObservedState,
        OperatorEvent, PlatformMetrics, SystemSnapshotEvent, SystemStatus, TradingSnapshotEvent,
        TradingStateSnapshot,
    };

    fn sample_snapshot() -> DashboardSnapshot {
        DashboardSnapshot {
            system: SystemStatus {
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
            },
            metrics: PlatformMetrics {
                total_deployments: 1,
                live_deployments: 0,
                degraded_deployments: 0,
                active_alerts: 1,
                stale_sources: 1,
                live_reconcile_failures: 0,
                host_cpu_pressure_milli_percent: None,
                host_load_average_1m_milli: None,
                process_memory_mb: None,
                host_memory_available_mb: None,
                last_trade_time: None,
                last_live_reconcile_success_at: None,
                heartbeats: vec![HeartbeatStatus {
                    source_id: "worker:example.paper".to_string(),
                    source_kind: "worker".to_string(),
                    state: HeartbeatState::Healthy,
                    last_seen_at: Some(Utc::now()),
                    stale_after_seconds: 15,
                    message: None,
                }],
            },
            alerts: vec![ActiveAlert {
                alert_id: "source-stale:live_reconcile".to_string(),
                kind: AlertKind::SourceStale,
                severity: AlertSeverity::Critical,
                source_id: "live_reconcile".to_string(),
                message: "live reconcile loop exceeded stale threshold".to_string(),
                triggered_at: Utc::now(),
            }],
            deployments: vec![DeploymentSummary {
                deployment_id: "example.paper".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                account_id: "acct-paper".to_string(),
                max_gross_exposure: None,
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
                observed_state: ObservedState::Running,
            }],
            trading: vec![TradingStateSnapshot {
                deployment_id: "example.paper".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                ..TradingStateSnapshot::default()
            }],
            recent_events: vec![
                OperatorEvent::SystemSnapshot(SystemSnapshotEvent {
                    system: SystemStatus {
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
                    },
                }),
                OperatorEvent::DeploymentSnapshot(DeploymentSnapshotEvent {
                    deployments: vec![DeploymentSummary {
                        deployment_id: "example.paper".to_string(),
                        runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                        account_id: "acct-paper".to_string(),
                        max_gross_exposure: None,
                        deployment_state: DeploymentState::Enabled,
                        desired_state: DesiredState::Running,
                        observed_state: ObservedState::Running,
                    }],
                }),
                OperatorEvent::TradingSnapshot(TradingSnapshotEvent {
                    trading: vec![TradingStateSnapshot {
                        deployment_id: "example.paper".to_string(),
                        runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                        ..TradingStateSnapshot::default()
                    }],
                }),
            ],
        }
    }

    #[test]
    fn dashboard_render_includes_core_sections() {
        let output = render_dashboard(&sample_snapshot());

        assert!(output.contains("PLOY TUI"));
        assert!(output.contains("System"));
        assert!(output.contains("Deployments"));
        assert!(output.contains("Trading"));
        assert!(output.contains("Metrics"));
        assert!(output.contains("Active Alerts"));
        assert!(output.contains("Recent Events"));
        assert!(output.contains("example.paper"));
        assert!(output.contains("running"));
    }

    #[test]
    fn event_render_labels_snapshot_types() {
        let output = render_event_line(&OperatorEvent::SystemSnapshot(SystemSnapshotEvent {
            system: SystemStatus {
                status: "running".to_string(),
                uptime_seconds: 1,
                version: "0.1.0".to_string(),
                strategy: "platform".to_string(),
                last_trade_time: Some(Utc::now()),
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
        }));

        assert!(output.contains("system_snapshot"));
        assert!(output.contains("running"));
    }
}
