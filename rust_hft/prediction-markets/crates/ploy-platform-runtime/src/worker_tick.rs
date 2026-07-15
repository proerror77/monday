use chrono::Duration;
use ploy_deployments::{WorkerLaunchSpec, WorkerSupervisor, CANONICAL_CONTROL_GENERATION};
use ploy_operator_contracts::{
    DeploymentRuntimeMode, DeploymentState, DesiredState, ObservedState,
};
use ploy_platform::{ControlPlane, DeploymentRecord};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct WorkerTickConfig {
    pub listen_addr: String,
    pub worker_heartbeat_stale_after_ms: u64,
    pub runner_binary: PathBuf,
    pub strategy_config_root: PathBuf,
    pub working_directory: PathBuf,
    pub canonical_live_ledgers: BTreeSet<String>,
}

pub fn build_worker_launch_spec(
    record: &DeploymentRecord,
    config: &WorkerTickConfig,
) -> WorkerLaunchSpec {
    let command = resolve_command_path(&config.runner_binary, &config.working_directory);
    let config_path = resolve_bundle_config_path(
        &record.bundle_id,
        &config.strategy_config_root,
        &config.working_directory,
    );

    let mut args = vec![
        "run".to_string(),
        "--config".to_string(),
        config_path.to_string_lossy().into_owned(),
        "--deployment-id".to_string(),
        record.deployment_id.clone(),
        "--foreground".to_string(),
        "--control-generation".to_string(),
        CANONICAL_CONTROL_GENERATION.to_string(),
    ];
    match record.runtime_mode {
        DeploymentRuntimeMode::Paper => args.push("--dry-run".to_string()),
        DeploymentRuntimeMode::Live => {}
    }

    WorkerLaunchSpec {
        deployment_id: record.deployment_id.clone(),
        bundle_id: record.bundle_id.clone(),
        runtime_mode: record.runtime_mode.clone(),
        desired_state: record.desired_state,
        command,
        args,
        working_directory: config.working_directory.clone(),
        pid_file: config
            .working_directory
            .join("run/platform/workers")
            .join(format!("{}.pid", record.deployment_id)),
    }
}

fn resolve_command_path(command: &Path, working_directory: &Path) -> PathBuf {
    if command.is_absolute() {
        command.to_path_buf()
    } else {
        working_directory.join(command)
    }
}

fn resolve_bundle_config_path(
    bundle_id: &str,
    strategy_config_root: &Path,
    working_directory: &Path,
) -> PathBuf {
    let raw = PathBuf::from(bundle_id);
    if raw.is_absolute() {
        return raw;
    }

    if bundle_id.contains('/') || bundle_id.ends_with(".toml") {
        return working_directory.join(raw);
    }

    strategy_config_root.join(format!("{bundle_id}.toml"))
}

pub fn tick_workers(
    control_plane: &mut ControlPlane,
    supervisor: &mut WorkerSupervisor,
    config: &WorkerTickConfig,
) {
    let records = control_plane.deployments.records();

    for record in records {
        if record.deployment_state == DeploymentState::Archived {
            control_plane
                .system
                .clear_source(&format!("worker:{}", record.deployment_id));
            if let Some(status) = supervisor.stop(&record.deployment_id) {
                control_plane
                    .deployments
                    .set_observed_state(&record.deployment_id, status.observed_state);
            } else {
                control_plane
                    .deployments
                    .set_observed_state(&record.deployment_id, ObservedState::Stopped);
            }
            continue;
        }
        if record.runtime_mode == DeploymentRuntimeMode::Live
            && !config
                .canonical_live_ledgers
                .contains(&record.deployment_id)
        {
            control_plane
                .system
                .clear_source(&format!("worker:{}", record.deployment_id));
            supervisor.terminate_pidfile_worker(build_worker_launch_spec(&record, config));
            control_plane
                .deployments
                .set_desired_state(&record.deployment_id, DesiredState::Paused);
            control_plane
                .deployments
                .set_observed_state(&record.deployment_id, ObservedState::Degraded);
            continue;
        }
        match record.desired_state {
            DesiredState::Running => {
                let status = supervisor
                    .status(&record.deployment_id)
                    .map(|status| status.observed_state);
                match status {
                    None => {
                        supervisor.start(build_worker_launch_spec(&record, config));
                    }
                    Some(
                        ObservedState::Paused | ObservedState::Stopped | ObservedState::Failed,
                    ) => {
                        supervisor.restart_with_spec(build_worker_launch_spec(&record, config));
                    }
                    Some(
                        ObservedState::Starting | ObservedState::Running | ObservedState::Degraded,
                    ) => {}
                }
                if let Some(status) = supervisor.heartbeat(&record.deployment_id) {
                    control_plane
                        .deployments
                        .set_observed_state(&record.deployment_id, status.observed_state);
                    control_plane.system.note_source_heartbeat(
                        format!("worker:{}", record.deployment_id),
                        "worker",
                        Duration::milliseconds(config.worker_heartbeat_stale_after_ms as i64),
                    );
                }
            }
            DesiredState::Paused => {
                control_plane
                    .system
                    .clear_source(&format!("worker:{}", record.deployment_id));
                if let Some(status) = supervisor.pause(&record.deployment_id) {
                    control_plane
                        .deployments
                        .set_observed_state(&record.deployment_id, status.observed_state);
                } else {
                    control_plane
                        .deployments
                        .set_observed_state(&record.deployment_id, ObservedState::Paused);
                }
            }
            DesiredState::Stopped => {
                control_plane
                    .system
                    .clear_source(&format!("worker:{}", record.deployment_id));
                if let Some(status) = supervisor.stop(&record.deployment_id) {
                    control_plane
                        .deployments
                        .set_observed_state(&record.deployment_id, status.observed_state);
                } else {
                    control_plane
                        .deployments
                        .set_observed_state(&record.deployment_id, ObservedState::Stopped);
                }
            }
        }
    }
}

pub fn refresh_source_health(control_plane: &mut ControlPlane, listen_addr: &str) {
    let stale_sources = control_plane.system.refresh_source_health();
    let records = control_plane.deployments.records();

    for record in records {
        if record.desired_state != DesiredState::Running {
            continue;
        }
        let worker_stale = control_plane
            .system
            .source_is_stale(&format!("worker:{}", record.deployment_id));
        let live_source_stale = record.runtime_mode == DeploymentRuntimeMode::Live
            && (control_plane.system.source_is_stale("live_reconcile")
                || control_plane.system.source_is_stale("venue:polymarket"));

        if worker_stale || live_source_stale {
            control_plane
                .deployments
                .set_observed_state(&record.deployment_id, ObservedState::Degraded);
        }
    }

    if stale_sources > 0 {
        control_plane.system.mark_degraded(listen_addr);
        return;
    }

    if control_plane.system.is_degraded() {
        control_plane.system.mark_recovering(listen_addr);
    }

    for record in control_plane.deployments.records() {
        if record.desired_state == DesiredState::Running
            && record.observed_state == ObservedState::Degraded
        {
            control_plane
                .deployments
                .set_observed_state(&record.deployment_id, ObservedState::Running);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{refresh_source_health, tick_workers, WorkerTickConfig};
    use ploy_deployments::WorkerSupervisor;
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
    use ploy_platform::{ControlPlane, DeploymentRecord};
    use rust_decimal_macros::dec;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::Duration as StdDuration;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn test_id() -> String {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let sequence = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        format!("{}-{unique}-{sequence}", std::process::id())
    }

    fn test_working_directory() -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ploy-platform-runtime-workdir-{}", test_id()));
        fs::create_dir_all(&path).expect("create test working directory");
        path
    }

    fn test_runner_binary() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ploy-platform-runtime-test-runner-{}.sh",
            test_id()
        ));
        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, "#!/bin/sh\nsleep 30\n").expect("write test runner");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755))
                .expect("chmod test runner");
        }
        fs::rename(&tmp_path, &path).expect("publish test runner");
        path
    }

    fn config() -> WorkerTickConfig {
        WorkerTickConfig {
            listen_addr: "127.0.0.1:8081".to_string(),
            worker_heartbeat_stale_after_ms: 15_000,
            runner_binary: test_runner_binary(),
            strategy_config_root: PathBuf::from("config/strategies"),
            working_directory: test_working_directory(),
            canonical_live_ledgers: BTreeSet::new(),
        }
    }

    #[test]
    fn tick_boots_running_workers_and_updates_status() {
        let mut control_plane = ControlPlane::default();
        let mut supervisor = WorkerSupervisor::default();
        let config = config();
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });

        tick_workers(&mut control_plane, &mut supervisor, &config);
        assert!(supervisor.status("example.paper").is_some());
    }

    #[test]
    fn tick_blocks_live_worker_start_without_canonical_ledger_presence() {
        let mut control_plane = ControlPlane::default();
        let mut supervisor = WorkerSupervisor::default();
        let config = config();
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "missing-ledger.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });

        tick_workers(&mut control_plane, &mut supervisor, &config);

        let record = control_plane
            .deployments
            .get("missing-ledger.live")
            .expect("deployment");
        assert_eq!(record.desired_state, DesiredState::Paused);
        assert_eq!(record.observed_state, ObservedState::Degraded);
        assert!(supervisor.status("missing-ledger.live").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn missing_live_ledger_terminates_legacy_pidfile_worker() {
        let working_directory = test_working_directory();
        let pid_file = working_directory.join("run/platform/workers/missing-ledger.live.pid");
        let ready_file = working_directory.join("legacy-worker.ready");
        fs::create_dir_all(pid_file.parent().expect("pid parent")).expect("pid parent");
        let mut legacy = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "touch \"$READY_FILE\"; sleep 30; :",
                "worker",
                "--deployment-id",
                "missing-ledger.live",
            ])
            .env("READY_FILE", &ready_file)
            .spawn()
            .expect("spawn legacy worker");
        let legacy_pid = legacy.id();
        let ready = (0..100).any(|_| {
            if ready_file.exists() {
                true
            } else {
                thread::sleep(StdDuration::from_millis(10));
                false
            }
        });
        assert!(ready, "legacy worker did not finish exec setup");
        fs::write(&pid_file, format!("{legacy_pid}\n")).expect("pidfile");
        let config = WorkerTickConfig {
            listen_addr: "127.0.0.1:8081".to_string(),
            worker_heartbeat_stale_after_ms: 15_000,
            runner_binary: PathBuf::from("/bin/sh"),
            strategy_config_root: PathBuf::from("config/strategies"),
            working_directory,
            canonical_live_ledgers: BTreeSet::new(),
        };
        let mut control_plane = ControlPlane::default();
        let mut supervisor = WorkerSupervisor::default();
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "missing-ledger.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });

        tick_workers(&mut control_plane, &mut supervisor, &config);

        let exited = (0..100).any(|_| match legacy.try_wait() {
            Ok(Some(_)) => true,
            Ok(None) => {
                thread::sleep(StdDuration::from_millis(10));
                false
            }
            Err(error) => panic!("wait legacy worker: {error}"),
        });
        if !exited {
            let _ = legacy.kill();
            let _ = legacy.wait();
        }
        assert!(
            exited,
            "legacy pidfile worker survived missing-ledger cutover"
        );
        assert!(!pid_file.exists());
        assert!(supervisor.status("missing-ledger.live").is_none());
    }

    #[test]
    fn stale_sources_degrade_and_then_recover() {
        let mut control_plane = ControlPlane::default();
        control_plane.system.mark_running("127.0.0.1:8081");
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        });
        control_plane.system.note_source_failure(
            "live_reconcile",
            "live_reconcile",
            chrono::Duration::seconds(15),
            "offline".to_string(),
        );

        refresh_source_health(&mut control_plane, "127.0.0.1:8081");
        assert!(control_plane.system.is_degraded());
    }

    #[test]
    fn build_launch_spec_resolves_strategy_config_path_and_dry_run_flag() {
        let config = config();
        let spec = super::build_worker_launch_spec(
            &DeploymentRecord {
                deployment_id: "pm5d.v2.paper".to_string(),
                bundle_id: "02-pm5d.v2-dryrun".to_string(),
                runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
                account_id: "acct-paper".to_string(),
                max_gross_exposure: Some(dec!(5)),
                deployment_state: DeploymentState::Enabled,
                desired_state: DesiredState::Running,
                observed_state: ObservedState::Starting,
            },
            &config,
        );

        assert!(spec
            .command
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("ploy-platform-runtime-test-runner-")));
        assert!(spec
            .args
            .contains(&"config/strategies/02-pm5d.v2-dryrun.toml".to_string()));
        let deployment_id_arg = spec
            .args
            .windows(2)
            .find(|window| window[0] == "--deployment-id")
            .map(|window| window[1].as_str());
        assert_eq!(deployment_id_arg, Some("pm5d.v2.paper"));
        assert!(spec.args.contains(&"--dry-run".to_string()));
        assert!(spec.args.windows(2).any(|window| {
            window[0] == "--control-generation"
                && window[1] == ploy_deployments::CANONICAL_CONTROL_GENERATION
        }));
    }

    #[test]
    fn repeated_ticks_do_not_respawn_running_worker() {
        let mut control_plane = ControlPlane::default();
        let mut supervisor = WorkerSupervisor::default();
        let config = config();
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });

        let first_pid = wait_for_worker_pid(&mut control_plane, &mut supervisor, &config);

        let second_pid = wait_for_worker_pid(&mut control_plane, &mut supervisor, &config);

        assert_eq!(first_pid, second_pid);
    }

    #[test]
    fn archived_worker_is_stopped() {
        let mut control_plane = ControlPlane::default();
        let mut supervisor = WorkerSupervisor::default();
        let config = config();
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });
        let pid = wait_for_worker_pid(&mut control_plane, &mut supervisor, &config);

        control_plane
            .deployments
            .set_deployment_state("example.paper", DeploymentState::Archived);
        tick_workers(&mut control_plane, &mut supervisor, &config);

        let status = supervisor.status("example.paper").expect("worker status");
        assert_eq!(status.observed_state, ObservedState::Stopped);
        assert_eq!(status.pid, None, "archived worker {pid} must be terminated");
    }

    fn wait_for_worker_pid(
        control_plane: &mut ControlPlane,
        supervisor: &mut WorkerSupervisor,
        config: &WorkerTickConfig,
    ) -> u32 {
        for _ in 0..20 {
            tick_workers(control_plane, supervisor, config);
            if let Some(pid) = supervisor
                .status("example.paper")
                .and_then(|status| status.pid)
            {
                return pid;
            }
            thread::sleep(StdDuration::from_millis(10));
        }
        let status = supervisor.status("example.paper");
        panic!("worker did not report a pid after bounded ticks; final status={status:?}");
    }

    #[test]
    fn running_desired_state_restarts_paused_worker() {
        let mut control_plane = ControlPlane::default();
        let mut supervisor = WorkerSupervisor::default();
        let config = config();
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });

        tick_workers(&mut control_plane, &mut supervisor, &config);
        let paused = supervisor.pause("example.paper").expect("paused worker");
        assert_eq!(paused.observed_state, ObservedState::Paused);

        let restarted = (0..20).any(|_| {
            tick_workers(&mut control_plane, &mut supervisor, &config);
            let Some(status) = supervisor.status("example.paper") else {
                return false;
            };
            if matches!(
                status.observed_state,
                ObservedState::Starting | ObservedState::Running
            ) && status.pid.is_some()
            {
                return true;
            }
            thread::sleep(StdDuration::from_millis(10));
            false
        });

        let status = supervisor.status("example.paper").expect("status");
        assert!(
            restarted,
            "worker did not restart from paused state; final status={status:?}"
        );
    }

    #[test]
    fn resume_replaces_stale_launch_spec_with_current_registry_spec() {
        let working_directory = test_working_directory();
        let launch_log = working_directory.join("launches.log");
        let runner = working_directory.join("recording-runner.sh");
        fs::write(
            &runner,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nsleep 30\n",
                launch_log.display()
            ),
        )
        .expect("write recording runner");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&runner, fs::Permissions::from_mode(0o755))
                .expect("chmod recording runner");
        }
        let config = WorkerTickConfig {
            listen_addr: "127.0.0.1:8081".to_string(),
            worker_heartbeat_stale_after_ms: 15_000,
            runner_binary: runner,
            strategy_config_root: PathBuf::from("config/strategies"),
            working_directory,
            canonical_live_ledgers: ["resume.current".to_string()].into_iter().collect(),
        };
        let mut control_plane = ControlPlane::default();
        let mut supervisor = WorkerSupervisor::default();
        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "resume.current".to_string(),
            bundle_id: "old-live".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });
        tick_workers(&mut control_plane, &mut supervisor, &config);
        let initial_launched = (0..1000).any(|_| {
            tick_workers(&mut control_plane, &mut supervisor, &config);
            if fs::read_to_string(&launch_log).is_ok_and(|launches| launches.lines().count() >= 1) {
                true
            } else {
                thread::sleep(StdDuration::from_millis(10));
                false
            }
        });
        assert!(
            initial_launched,
            "initial stale-spec launch was not recorded"
        );
        supervisor.pause("resume.current").expect("pause worker");

        control_plane.deployments.upsert(DeploymentRecord {
            deployment_id: "resume.current".to_string(),
            bundle_id: "new-paper".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });
        tick_workers(&mut control_plane, &mut supervisor, &config);

        let launches = (0..1000)
            .find_map(|_| {
                tick_workers(&mut control_plane, &mut supervisor, &config);
                if let Ok(launches) = fs::read_to_string(&launch_log) {
                    if launches.lines().count() >= 2 {
                        return Some(launches);
                    }
                }
                thread::sleep(StdDuration::from_millis(10));
                None
            })
            .expect("resumed launch record");
        let resumed = launches.lines().last().expect("resumed launch");
        assert!(resumed.contains("config/strategies/new-paper.toml"));
        assert!(resumed.contains("--dry-run"));
        assert!(!resumed.contains("old-live"));
        supervisor.stop("resume.current");
    }
}
