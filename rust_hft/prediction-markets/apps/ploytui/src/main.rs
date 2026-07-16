use ploy_control_client::ControlPlaneClient;
use ploytui::{render_dashboard, DashboardSnapshot};
use std::thread;
use std::time::Duration;

fn parse_watch_flag(args: &[String]) -> bool {
    args.iter().skip(1).any(|arg| arg == "--watch")
}

fn build_snapshot(client: &ControlPlaneClient) -> DashboardSnapshot {
    let system =
        client
            .system_snapshot()
            .unwrap_or_else(|_| ploy_operator_contracts::SystemStatus {
                status: "unavailable".to_string(),
                uptime_seconds: 0,
                version: "unknown".to_string(),
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
            });
    let deployments = client.list_deployments();
    DashboardSnapshot {
        metrics: client.system_metrics().unwrap_or_else(|_| {
            ploy_operator_contracts::PlatformMetrics {
                total_deployments: deployments.len(),
                live_deployments: 0,
                degraded_deployments: deployments
                    .iter()
                    .filter(|deployment| {
                        deployment.observed_state
                            == ploy_operator_contracts::ObservedState::Degraded
                    })
                    .count(),
                active_alerts: system.active_alert_count,
                stale_sources: system.stale_source_count,
                live_reconcile_failures: system.live_reconcile_failures,
                host_cpu_pressure_milli_percent: None,
                host_load_average_1m_milli: None,
                process_memory_mb: None,
                host_memory_available_mb: None,
                last_trade_time: system.last_trade_time,
                last_live_reconcile_success_at: system.last_live_reconcile_success_at,
                heartbeats: Vec::new(),
            }
        }),
        alerts: client.system_alerts().unwrap_or_default(),
        system,
        deployments,
        trading: client.trading_state().unwrap_or_default(),
        recent_events: client.recent_events(6).unwrap_or_default(),
    }
}

fn main() {
    let watch = parse_watch_flag(&std::env::args().collect::<Vec<_>>());
    let client = ControlPlaneClient::default();

    loop {
        let snapshot = build_snapshot(&client);
        print!("\x1B[2J\x1B[H{}", render_dashboard(&snapshot));
        if !watch {
            break;
        }
        thread::sleep(Duration::from_secs(2));
    }
}
