use ploy_deployments::{WorkerLaunchSpec, WorkerSupervisor};
use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
use ploy_platform::{ControlPlane, DeploymentRecord};
use std::path::PathBuf;

#[test]
fn platform_smoke_registers_and_starts_one_deployment() {
    let mut control_plane = ControlPlane::default();
    control_plane.deployments.upsert(DeploymentRecord {
        deployment_id: "example.paper".to_string(),
        bundle_id: "openclaw".to_string(),
        runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
        account_id: "acct-paper".to_string(),
        max_gross_exposure: Some(rust_decimal::Decimal::new(500, 2)),
        deployment_state: DeploymentState::Enabled,
        desired_state: DesiredState::Running,
        observed_state: ObservedState::Starting,
    });

    let mut supervisor = WorkerSupervisor::default();
    supervisor.start(WorkerLaunchSpec {
        deployment_id: "example.paper".to_string(),
        bundle_id: "openclaw".to_string(),
        runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
        desired_state: DesiredState::Running,
        command: PathBuf::from("/bin/sh"),
        args: vec!["-lc".to_string(), "sleep 30".to_string()],
        working_directory: std::env::current_dir().expect("cwd"),
        pid_file: std::env::temp_dir().join("ploy-platform-smoke.pid"),
    });
    supervisor.heartbeat("example.paper");

    let summary = control_plane
        .deployments
        .summaries()
        .into_iter()
        .next()
        .expect("deployment summary");
    assert_eq!(summary.deployment_id, "example.paper");
    assert_eq!(summary.desired_state, DesiredState::Running);
    assert_eq!(supervisor.workers().count(), 1);
    assert_eq!(
        supervisor.workers().next().expect("worker").observed_state,
        ObservedState::Running
    );

    supervisor.stop("example.paper");
}
