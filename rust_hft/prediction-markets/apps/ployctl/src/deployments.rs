use crate::client::ControlPlaneClient;
use ploy_operator_contracts::{DeploymentApplyRequest, DeploymentState, DesiredState};
use std::fs;
use std::path::Path;

pub fn render_deployments(client: &ControlPlaneClient) -> Result<String, String> {
    Ok(client
        .deployment_summaries()?
        .into_iter()
        .map(|deployment| {
            format!(
                "{} account={} max_gross_exposure={} mode={:?} lifecycle={:?} desired={:?} observed={:?}",
                deployment.deployment_id,
                deployment.account_id,
                deployment
                    .max_gross_exposure
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                deployment.runtime_mode,
                deployment.deployment_state,
                deployment.desired_state,
                deployment.observed_state
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn render_deployment(
    client: &ControlPlaneClient,
    deployment_id: &str,
) -> Result<String, String> {
    client.inspect_deployment(deployment_id).map(|deployment| {
        format!(
            "{} account={} max_gross_exposure={} mode={:?} lifecycle={:?} desired={:?} observed={:?}",
            deployment.deployment_id,
            deployment.account_id,
            deployment
                .max_gross_exposure
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            deployment.runtime_mode,
            deployment.deployment_state,
            deployment.desired_state,
            deployment.observed_state
        )
    })
}

pub fn apply_deployment_file(
    client: &ControlPlaneClient,
    manifest_path: &Path,
) -> Result<String, String> {
    let body = fs::read_to_string(manifest_path).map_err(|err| {
        format!(
            "read deployment manifest {}: {err}",
            manifest_path.display()
        )
    })?;
    let request: DeploymentApplyRequest =
        serde_json::from_str(&body).map_err(|err| format!("parse deployment manifest: {err}"))?;
    let deployment = client.apply_deployment(&request)?;
    Ok(format!(
        "{} account={} max_gross_exposure={} mode={:?} lifecycle={:?} desired={:?} observed={:?}",
        deployment.deployment_id,
        deployment.account_id,
        deployment
            .max_gross_exposure
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        deployment.runtime_mode,
        deployment.deployment_state,
        deployment.desired_state,
        deployment.observed_state
    ))
}

pub fn control_deployment(
    client: &ControlPlaneClient,
    deployment_id: &str,
    desired_state: DesiredState,
) -> Result<String, String> {
    let deployment = client.set_desired_state(deployment_id, desired_state)?;
    Ok(format!(
        "{} account={} max_gross_exposure={} mode={:?} lifecycle={:?} desired={:?} observed={:?}",
        deployment.deployment_id,
        deployment.account_id,
        deployment
            .max_gross_exposure
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        deployment.runtime_mode,
        deployment.deployment_state,
        deployment.desired_state,
        deployment.observed_state
    ))
}

pub fn set_lifecycle_state(
    client: &ControlPlaneClient,
    deployment_id: &str,
    deployment_state: DeploymentState,
) -> Result<String, String> {
    let deployment = client.set_deployment_state(deployment_id, deployment_state)?;
    Ok(format!(
        "{} account={} max_gross_exposure={} lifecycle={:?} desired={:?} observed={:?}",
        deployment.deployment_id,
        deployment.account_id,
        deployment
            .max_gross_exposure
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string()),
        deployment.deployment_state,
        deployment.desired_state,
        deployment.observed_state
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_deployment_file, control_deployment, render_deployment, render_deployments,
        set_lifecycle_state,
    };
    use crate::client::ControlPlaneClient;
    use ploy_operator_contracts::{DeploymentState, DesiredState};
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
        std::env::temp_dir().join(format!("ployctl-deployments-{label}-{unique}"))
    }

    #[test]
    fn list_deployments() {
        let runtime_root = temp_dir("list");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "deployment_state": "enabled",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("write deployments");

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = "127.0.0.1:9".to_string();
        let output = render_deployments(&client).expect("list deployments");
        assert!(output.contains("example.paper"));
    }

    #[test]
    fn inspect_one_deployment() {
        let runtime_root = temp_dir("inspect");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::json!([
                {
                    "deployment_id": "example.paper",
                    "deployment_state": "enabled",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("write deployments");

        let mut client = ControlPlaneClient::from_runtime_root(&runtime_root);
        client.control_plane_addr = "127.0.0.1:9".to_string();
        let output = render_deployment(&client, "example.paper").expect("deployment");
        assert!(output.contains("example.paper"));
    }

    #[test]
    fn apply_and_control_deployment_commands() {
        let runtime_root = temp_dir("mutate");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        let manifest = runtime_root.join("example.paper.json");
        fs::write(
            &manifest,
            serde_json::json!({
                "deployment_id": "example.paper",
                "bundle_id": "example",
                "runtime_mode": "paper",
                "deployment_state": "enabled",
                "desired_state": "running"
            })
            .to_string(),
        )
        .expect("write manifest");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0_u8; 2048];
                let bytes = stream.read(&mut request).expect("read request");
                let request = String::from_utf8_lossy(&request[..bytes]);
                let body = if request.starts_with("PUT /api/deployments/example.paper") {
                    serde_json::json!({
                        "deployment_id": "example.paper",
                        "deployment_state": "enabled",
                        "desired_state": "running",
                        "observed_state": "starting"
                    })
                    .to_string()
                } else if request.contains("\"deployment_state\":\"draining\"") {
                    serde_json::json!({
                        "deployment_id": "example.paper",
                        "deployment_state": "draining",
                        "desired_state": "paused",
                        "observed_state": "paused"
                    })
                    .to_string()
                } else {
                    serde_json::json!({
                        "deployment_id": "example.paper",
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

        let applied = apply_deployment_file(&client, &manifest).expect("apply command");
        assert!(applied.contains("example.paper"));

        let paused =
            control_deployment(&client, "example.paper", DesiredState::Paused).expect("pause");
        assert!(paused.contains("Paused"));
        let draining = set_lifecycle_state(&client, "example.paper", DeploymentState::Draining)
            .expect("drain");
        assert!(draining.contains("Draining"));
    }

    #[test]
    fn list_deployments_returns_structured_http_error_instead_of_empty_success() {
        let runtime_root = temp_dir("list-http-error");
        fs::create_dir_all(&runtime_root).expect("create runtime root");
        fs::write(
            runtime_root.join("deployments.json"),
            serde_json::json!([
                {
                    "deployment_id": "stale.paper",
                    "deployment_state": "enabled",
                    "desired_state": "running",
                    "observed_state": "running"
                }
            ])
            .to_string(),
        )
        .expect("write stale deployments");

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

        let error = render_deployments(&client).expect_err("structured list error");
        assert!(error.contains("daemon_lock_poisoned"));
        assert!(!error.contains("stale.paper"));
    }
}
