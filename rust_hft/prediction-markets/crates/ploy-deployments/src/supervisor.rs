use crate::health::heartbeat;
use crate::protocol::{WorkerLaunchSpec, WorkerStatus};
use crate::runtime::terminate_pidfile_worker;
use crate::runtime::DeploymentRuntime;
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct WorkerSupervisor {
    workers: BTreeMap<String, DeploymentRuntime>,
}

impl WorkerSupervisor {
    pub fn start(&mut self, spec: WorkerLaunchSpec) -> &WorkerStatus {
        let deployment_id = spec.deployment_id.clone();
        let runtime = DeploymentRuntime::new(spec);
        self.workers.insert(deployment_id.clone(), runtime);
        self.workers
            .get(&deployment_id)
            .expect("worker runtime")
            .boot_status()
    }

    pub fn heartbeat(&mut self, deployment_id: &str) -> Option<&WorkerStatus> {
        let runtime = self.workers.get_mut(deployment_id)?;
        let status = runtime.refresh_status();
        heartbeat(status);
        Some(status)
    }

    pub fn status(&self, deployment_id: &str) -> Option<&WorkerStatus> {
        self.workers
            .get(deployment_id)
            .map(DeploymentRuntime::status)
    }

    pub fn fail(&mut self, deployment_id: &str) -> Option<&WorkerStatus> {
        let runtime = self.workers.get_mut(deployment_id)?;
        Some(runtime.fail())
    }

    pub fn pause(&mut self, deployment_id: &str) -> Option<&WorkerStatus> {
        let runtime = self.workers.get_mut(deployment_id)?;
        Some(runtime.pause())
    }

    pub fn stop(&mut self, deployment_id: &str) -> Option<&WorkerStatus> {
        let runtime = self.workers.get_mut(deployment_id)?;
        Some(runtime.stop())
    }

    pub fn restart(&mut self, deployment_id: &str) -> Option<&WorkerStatus> {
        let runtime = self.workers.get_mut(deployment_id)?;
        let status = runtime.restart();
        heartbeat(status);
        Some(status)
    }

    pub fn restart_with_spec(&mut self, spec: WorkerLaunchSpec) -> &WorkerStatus {
        self.stop(&spec.deployment_id);
        self.start(spec)
    }

    pub fn terminate_pidfile_worker(&mut self, spec: WorkerLaunchSpec) -> bool {
        if self.stop(&spec.deployment_id).is_some() {
            true
        } else {
            terminate_pidfile_worker(&spec)
        }
    }

    pub fn workers(&self) -> impl Iterator<Item = &WorkerStatus> {
        self.workers.values().map(DeploymentRuntime::status)
    }
}

#[cfg(test)]
mod tests {
    use super::WorkerSupervisor;
    use crate::protocol::WorkerLaunchSpec;
    use ploy_operator_contracts::{DeploymentRuntimeMode, DesiredState, ObservedState};
    use std::path::PathBuf;

    fn test_launch_spec() -> WorkerLaunchSpec {
        WorkerLaunchSpec {
            deployment_id: "openclaw.default".to_string(),
            bundle_id: "openclaw".to_string(),
            runtime_mode: DeploymentRuntimeMode::Paper,
            desired_state: DesiredState::Running,
            command: PathBuf::from("/bin/sh"),
            args: vec!["-lc".to_string(), "sleep 30".to_string()],
            working_directory: std::env::current_dir().expect("cwd"),
            pid_file: std::env::temp_dir().join("ploy-deployments-supervisor.pid"),
        }
    }

    #[test]
    fn start_one_worker() {
        let mut supervisor = WorkerSupervisor::default();
        let status = supervisor.start(test_launch_spec());

        assert!(matches!(
            status.observed_state,
            ObservedState::Starting | ObservedState::Running
        ));
        assert!(status.pid.is_some());
    }

    #[test]
    fn restart_failed_worker() {
        let mut supervisor = WorkerSupervisor::default();
        supervisor.start(test_launch_spec());

        supervisor.fail("openclaw.default");
        let restarted = supervisor.restart("openclaw.default").expect("restarted");
        assert_eq!(restarted.observed_state, ObservedState::Running);
    }
}
