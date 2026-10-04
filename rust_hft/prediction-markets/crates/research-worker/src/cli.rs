use crate::{prediction_runner, prediction_snapshot};
use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use std::{ffi::OsString, path::PathBuf};
const BUILD_SOURCE_REVISION: &str = match option_env!("MONDAY_SOURCE_REVISION") {
    Some(value) => value,
    None => "unbound-source-revision",
};
#[derive(Debug, Parser)]
#[command(name = "monday-prediction-worker", version = BUILD_SOURCE_REVISION,
          about = "Governed prediction-market artifact worker")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, Subcommand)]
enum Command {
    Execute(Box<PredictionExecuteArgs>),
    Snapshot(PredictionSnapshotArgs),
}
#[derive(Debug, Clone, Args)]
pub struct PredictionExecuteArgs {
    #[arg(long)]
    pub work_dir: PathBuf,
    #[arg(long)]
    pub mission_url: String,
    #[arg(long)]
    pub mission_sha256: String,
    #[arg(long)]
    pub snapshot_url: String,
    #[arg(long)]
    pub snapshot_sha256: String,
    #[arg(long)]
    pub snapshot_contract_id: String,
    #[arg(long)]
    pub snapshot_digest: String,
    #[arg(long)]
    pub cohort_manifest_id: String,
    #[arg(long)]
    pub partition_digest: String,
    #[arg(long)]
    pub policy_identity: String,
    #[arg(long)]
    pub task_capability: String,
    #[arg(long)]
    pub image_identity: String,
    #[arg(long)]
    pub partition_view_json: String,
    /// Read-only cache directory containing `<snapshot-sha256>.zip` archives.
    #[arg(long)]
    pub snapshot_cache_dir: Option<PathBuf>,
    /// Prior immutable prediction result bundle for a paused LoopRun.
    #[arg(long, requires = "resume_sha256")]
    pub resume_url: Option<String>,
    #[arg(long, requires = "resume_url")]
    pub resume_sha256: Option<String>,
    #[arg(long)]
    pub result_put_url: String,
    /// Independently authorized read URL for the immutable published result bundle.
    #[arg(long)]
    pub result_readback_url: String,
}

#[derive(Debug, Clone, Args)]
pub struct PredictionSnapshotArgs {
    #[arg(long)]
    pub work_dir: PathBuf,
    #[arg(long)]
    pub result_put_url: String,
    /// Arguments forwarded to the governed snapshot compiler. `--output-dir`
    /// is owned by the prediction worker and must not be supplied here.
    #[arg(last = true, required = true, allow_hyphen_values = true)]
    pub compiler_args: Vec<OsString>,
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Execute(args) => {
            tokio::task::spawn_blocking(move || prediction_runner::execute(*args))
                .await
                .context("prediction execution worker failed")?
        }
        Command::Snapshot(args) => {
            tokio::task::spawn_blocking(move || prediction_snapshot::snapshot(args))
                .await
                .context("prediction snapshot worker failed")?
        }
    }
}
pub fn print_json(value: &impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    static PREDICTION_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prediction_execute_runs_blocking_pipeline_outside_async_runtime() {
        let _lock = PREDICTION_ENV_LOCK.lock().await;
        let root = tempfile::tempdir().unwrap();
        let runner = root.path().join("unused-prediction-runner");
        std::fs::write(&runner, b"unused").unwrap();
        let _runner = EnvVarGuard::set("MONDAY_PREDICTION_RESEARCH_BIN", &runner);
        let missing_mission = root.path().join("missing-mission.json");
        let cli = Cli::try_parse_from([
            OsString::from("monday-prediction-worker"),
            OsString::from("execute"),
            OsString::from("--work-dir"),
            root.path().join("work").into_os_string(),
            OsString::from("--mission-url"),
            missing_mission.clone().into_os_string(),
            OsString::from("--mission-sha256"),
            OsString::from("a".repeat(64)),
            OsString::from("--snapshot-url"),
            root.path().join("missing-snapshot.zip").into_os_string(),
            OsString::from("--snapshot-sha256"),
            OsString::from("b".repeat(64)),
            OsString::from("--snapshot-contract-id"),
            OsString::from(format!("sha256:{}", "c".repeat(64))),
            OsString::from("--snapshot-digest"),
            OsString::from("0123456789abcdef"),
            OsString::from("--cohort-manifest-id"),
            OsString::from(format!("sha256:{}", "d".repeat(64))),
            OsString::from("--partition-digest"),
            OsString::from(format!("sha256:{}", "e".repeat(64))),
            OsString::from("--policy-identity"),
            OsString::from(format!("sha256:{}", "f".repeat(64))),
            OsString::from("--task-capability"),
            OsString::from("btc_5m_backtest"),
            OsString::from("--image-identity"),
            OsString::from(format!("sha256:{}", "a".repeat(64))),
            OsString::from("--partition-view-json"),
            OsString::from(
                serde_json::json!({
                    "common_time_boundary_ms": 1,
                    "train_market_ids": ["train"],
                    "crossing_excluded_market_ids": [],
                    "held_out_market_ids": ["held-out"]
                })
                .to_string(),
            ),
            OsString::from("--result-put-url"),
            root.path().join("results.zip").into_os_string(),
            OsString::from("--result-readback-url"),
            root.path().join("results.zip").into_os_string(),
        ])
        .unwrap();

        let error = run(cli).await.unwrap_err();

        assert!(
            format!("{error:#}").contains(&format!(
                "failed to open local source {}",
                missing_mission.display()
            )),
            "unexpected error: {error:#}"
        );
    }
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn prediction_snapshot_runs_blocking_pipeline_outside_async_runtime() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = PREDICTION_ENV_LOCK.lock().await;
        let root = tempfile::tempdir().unwrap();
        let compiler = root.path().join("snapshot-compiler.sh");
        std::fs::write(
            &compiler,
            format!(
                r#"#!/bin/sh
set -eu
test "$1" = "--output-dir"
mkdir -p "$2"
printf '%s\n' '{{"schema_version":"research_snapshot_v2","snapshot_hash":"0123456789abcdef","snapshot_contract_hash":"sha256:{}"}}' > "$2/manifest.json"
"#,
                "1".repeat(64)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _compiler = EnvVarGuard::set("MONDAY_PREDICTION_SNAPSHOT_BIN", &compiler);
        let published = root.path().join("published-snapshot.zip");
        std::fs::write(&published, b"occupied").unwrap();
        let cli = Cli::try_parse_from([
            OsString::from("monday-prediction-worker"),
            OsString::from("snapshot"),
            OsString::from("--work-dir"),
            root.path().join("work").into_os_string(),
            OsString::from("--result-put-url"),
            published.clone().into_os_string(),
            OsString::from("--"),
            OsString::from("--start-date"),
            OsString::from("2026-07-01"),
        ])
        .unwrap();

        let error = run(cli).await.unwrap_err();

        assert!(
            format!("{error:#}").contains(&format!(
                "result destination already exists: {}",
                published.display()
            )),
            "unexpected error: {error:#}"
        );
    }
}
