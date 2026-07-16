use crate::client::ControlPlaneClient;
use ploy_operator_contracts::{AlertKind, AlertSeverity, HeartbeatState};

pub fn render_system_status(client: &ControlPlaneClient) -> Result<String, String> {
    let status = client.system_snapshot()?;
    Ok(format!(
        "status={} uptime={}s version={} db_connected={} ws_connected={} errors_1h={} active_alerts={} stale_sources={} last_trade_time={} live_reconcile_failures={} next_live_reconcile_at={} last_live_reconcile_error={} last_live_reconcile_success_at={}",
        status.status,
        status.uptime_seconds,
        status.version,
        status.database_connected,
        status.websocket_connected,
        status.error_count_1h,
        status.active_alert_count,
        status.stale_source_count,
        status
            .last_trade_time
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        status.live_reconcile_failures,
        status
            .next_live_reconcile_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        status
            .last_live_reconcile_error
            .unwrap_or_else(|| "-".to_string()),
        status
            .last_live_reconcile_success_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
    ))
}

pub fn render_audit_log(client: &ControlPlaneClient) -> Result<String, String> {
    Ok(client
        .audit_logs()?
        .into_iter()
        .map(|entry| {
            format!(
                "{} {} {} status={} auth={} required={} outcome={} client={} {}",
                entry.timestamp.to_rfc3339(),
                entry.method,
                entry.path,
                entry.status_code,
                entry.auth_level,
                entry.required_access,
                entry.outcome,
                entry.client_addr.unwrap_or_else(|| "-".to_string()),
                entry.message.unwrap_or_else(|| "-".to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn render_system_metrics(client: &ControlPlaneClient) -> Result<String, String> {
    let metrics = client.system_metrics()?;
    let heartbeat_summary = if metrics.heartbeats.is_empty() {
        "-".to_string()
    } else {
        metrics
            .heartbeats
            .iter()
            .map(|heartbeat| {
                format!(
                    "{}:{}:{}",
                    heartbeat.source_kind,
                    heartbeat.source_id,
                    heartbeat_state_name(heartbeat.state)
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    };

    Ok(format!(
        "deployments_total={} deployments_live={} deployments_degraded={} active_alerts={} stale_sources={} live_reconcile_failures={} host_cpu_pressure_percent={} host_load_average_1m={} process_memory_mb={} host_memory_available_mb={} last_trade_time={} last_live_reconcile_success_at={} heartbeats={}",
        metrics.total_deployments,
        metrics.live_deployments,
        metrics.degraded_deployments,
        metrics.active_alerts,
        metrics.stale_sources,
        metrics.live_reconcile_failures,
        metrics
            .host_cpu_pressure_milli_percent
            .map(|value| format!("{:.1}", value as f64 / 1000.0))
            .unwrap_or_else(|| "-".to_string()),
        metrics
            .host_load_average_1m_milli
            .map(|value| format!("{:.2}", value as f64 / 1000.0))
            .unwrap_or_else(|| "-".to_string()),
        metrics
            .process_memory_mb
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        metrics
            .host_memory_available_mb
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        metrics
            .last_trade_time
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        metrics
            .last_live_reconcile_success_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        heartbeat_summary,
    ))
}

pub fn render_system_alerts(client: &ControlPlaneClient) -> Result<String, String> {
    let alerts = client.system_alerts()?;
    if alerts.is_empty() {
        return Ok("none".to_string());
    }

    Ok(alerts
        .into_iter()
        .map(|alert| {
            format!(
                "{} {} {} {} {}",
                alert.triggered_at.to_rfc3339(),
                alert_severity_name(alert.severity),
                alert_kind_name(alert.kind),
                alert.source_id,
                alert.message,
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn heartbeat_state_name(state: HeartbeatState) -> &'static str {
    match state {
        HeartbeatState::Healthy => "healthy",
        HeartbeatState::Stale => "stale",
    }
}

fn alert_severity_name(severity: AlertSeverity) -> &'static str {
    match severity {
        AlertSeverity::Warning => "warning",
        AlertSeverity::Critical => "critical",
    }
}

fn alert_kind_name(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::SourceStale => "source_stale",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        render_audit_log, render_system_alerts, render_system_metrics, render_system_status,
    };
    use crate::client::ControlPlaneClient;
    use chrono::Utc;
    use ploy_operator_contracts::{ActiveAlert, AlertKind, AlertSeverity};
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
        std::env::temp_dir().join(format!("ployctl-system-{label}-{unique}"))
    }

    #[test]
    fn renders_snapshot_backed_system_status() {
        let runtime_root = temp_dir("status");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("system-status.json"),
            serde_json::json!({
                "status": "running",
                "uptime_seconds": 3,
                "version": "0.1.0",
                "strategy": "platform",
                "last_trade_time": null,
                "websocket_connected": false,
                "database_connected": false,
                "error_count_1h": 0,
                "live_reconcile_failures": 0,
                "next_live_reconcile_at": null,
                "last_live_reconcile_error": null,
                "active_alert_count": 1,
                "stale_source_count": 2,
                "last_live_reconcile_success_at": null
            })
            .to_string(),
        )
        .expect("write status");

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = "127.0.0.1:9".to_string();
        let output = render_system_status(&client).expect("system status");
        assert!(output.contains("running"));
        assert!(output.contains("live_reconcile_failures=0"));
        assert!(output.contains("active_alerts=1"));
        assert!(output.contains("stale_sources=2"));
    }

    #[test]
    fn renders_audit_log_lines_from_http() {
        let runtime_root = temp_dir("audit");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::json!([
                {
                    "timestamp": Utc::now(),
                    "method": "POST",
                    "path": "/api/deployments/example.paper/control",
                    "client_addr": "127.0.0.1:9000",
                    "auth_level": "admin",
                    "required_access": "admin",
                    "status_code": 200,
                    "outcome": "allowed",
                    "message": "deployment paused"
                }
            ])
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let output = render_audit_log(&client).expect("audit log");
        assert!(output.contains("/api/deployments/example.paper/control"));
        assert!(output.contains("deployment paused"));
        assert!(output.contains("auth=admin"));
    }

    #[test]
    fn renders_metrics_from_http() {
        let runtime_root = temp_dir("metrics");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::json!({
                "total_deployments": 3,
                "live_deployments": 2,
                "degraded_deployments": 1,
                "active_alerts": 2,
                "stale_sources": 1,
                "live_reconcile_failures": 4,
                "last_trade_time": null,
                "last_live_reconcile_success_at": null,
                "heartbeats": [{
                    "source_id": "live_reconcile",
                    "source_kind": "live_reconcile",
                    "state": "stale",
                    "last_seen_at": null,
                    "stale_after_seconds": 15,
                    "message": "live reconcile loop exceeded stale threshold"
                }]
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

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let output = render_system_metrics(&client).expect("metrics");
        assert!(output.contains("deployments_total=3"));
        assert!(output.contains("active_alerts=2"));
        assert!(output.contains("heartbeats=live_reconcile:live_reconcile:stale"));
    }

    #[test]
    fn renders_alerts_from_http() {
        let runtime_root = temp_dir("alerts");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = serde_json::to_string(&vec![ActiveAlert {
                alert_id: "source-stale:live_reconcile".to_string(),
                kind: AlertKind::SourceStale,
                severity: AlertSeverity::Critical,
                source_id: "live_reconcile".to_string(),
                message: "live reconcile loop exceeded stale threshold".to_string(),
                triggered_at: Utc::now(),
            }])
            .expect("body");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let output = render_system_alerts(&client).expect("alerts");
        assert!(output.contains("critical"));
        assert!(output.contains("source_stale"));
        assert!(output.contains("live_reconcile"));
    }

    #[test]
    fn renders_no_alerts_message_when_alert_list_is_empty() {
        let runtime_root = temp_dir("alerts-empty");
        fs::create_dir_all(&runtime_root).expect("create runtime root");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = "[]";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = addr.to_string();

        let output = render_system_alerts(&client).expect("alerts");
        assert_eq!(output, "none");
    }
}
