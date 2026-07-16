use std::path::Path;

fn load_env_file_if_present(path: &Path) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || std::env::var_os(key).is_some() {
            continue;
        }
        let mut value = value.trim().to_string();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = value[1..value.len() - 1].to_string();
        }
        std::env::set_var(key, value);
    }
}

fn main() {
    load_env_file_if_present(Path::new(".env"));
    let config = ploy_daemon_host::config::PlatformConfig::from_env();
    let daemon = std::sync::Arc::new(std::sync::Mutex::new(
        ploy_daemon_host::runtime::PloyDaemon::boot(&config).expect("boot new-ployd"),
    ));
    let events = std::sync::Arc::new(ploy_daemon_host::events::EventBroker::default());
    {
        let mut daemon = daemon.lock().expect("daemon lock");
        if let Err(err) = daemon.write_runtime_snapshots() {
            eprintln!("new-ployd boot degraded: {err}");
        }
        ploy_daemon_host::http::publish_snapshot_events(&daemon, &events);
    }
    let state = std::sync::Arc::new(ploy_daemon_host::http::AppState {
        daemon: daemon.clone(),
        events: events.clone(),
    });
    let _server = ploy_daemon_host::http::spawn_server(state).expect("start new-ployd http server");
    let daemon_guard = daemon.lock().expect("daemon lock");
    let status = daemon_guard.control_plane.system.status();
    let worker_count = daemon_guard.supervisor.workers().count();
    eprintln!("new-ployd booted");
    eprintln!("{}", ploy_daemon_host::http::render_status(&status));
    eprintln!("workers={worker_count}");
    drop(daemon_guard);
    ploy_daemon_host::runtime::run_shared_forever(daemon, events).expect("run new-ployd");
}

#[cfg(test)]
mod tests {
    use super::load_env_file_if_present;
    use std::path::Path;

    #[test]
    fn loads_missing_env_keys_from_file_without_overwriting_existing_values() {
        let tmp = std::env::temp_dir().join(format!(
            "new-ployd-env-{}.env",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("duration")
                .as_nanos()
        ));

        std::fs::write(
            &tmp,
            "DATABASE_URL=postgres://example\nPLOY_RUNTIME_ROOT=\"/opt/ploy/run/platform\"\nKEEP=from-file\n",
        )
        .expect("write env file");
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("PLOY_RUNTIME_ROOT");
        std::env::set_var("KEEP", "preexisting");

        load_env_file_if_present(Path::new(&tmp));

        assert_eq!(
            std::env::var("DATABASE_URL").as_deref(),
            Ok("postgres://example")
        );
        assert_eq!(
            std::env::var("PLOY_RUNTIME_ROOT").as_deref(),
            Ok("/opt/ploy/run/platform")
        );
        assert_eq!(std::env::var("KEEP").as_deref(), Ok("preexisting"));

        let _ = std::fs::remove_file(tmp);
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("PLOY_RUNTIME_ROOT");
        std::env::remove_var("KEEP");
    }
}
