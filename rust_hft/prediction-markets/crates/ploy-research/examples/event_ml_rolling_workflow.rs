use std::env;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::Serialize;

fn main() -> Result<()> {
    let args = env::args().collect::<Vec<_>>();
    if has_flag(&args, "--help") || has_flag(&args, "-h") {
        print_usage();
        return Ok(());
    }

    let config = Config::parse(&args)?;
    fs::create_dir_all(&config.output_root)
        .with_context(|| format!("create output root {}", config.output_root.display()))?;

    let mut prior_run_dirs = Vec::new();
    let mut windows = Vec::new();
    for (index, dataset) in config.datasets.iter().enumerate() {
        let window_id = index + 1;
        let output_dir = config
            .output_root
            .join(format!("window_{window_id:03}_event_ml"));
        let command_args = event_ml_workflow_args(&config, dataset, &output_dir, &prior_run_dirs);
        let command = event_ml_workflow_command_string(&command_args);

        eprintln!();
        eprintln!("--- rolling_window={window_id} ---");
        eprintln!("dataset={dataset}");
        eprintln!("output_dir={}", output_dir.display());
        eprintln!("prior_run_dirs={}", prior_run_dirs.len());
        eprintln!("command={command}");

        let status = if config.dry_run {
            "dry_run".to_string()
        } else {
            fs::create_dir_all(&output_dir)
                .with_context(|| format!("create output dir {}", output_dir.display()))?;
            let status = event_ml_workflow_command(&command_args)
                .status()
                .with_context(|| format!("spawn rolling window {window_id}"))?;
            ensure_success(window_id, status)?;
            "passed".to_string()
        };

        windows.push(RollingWindowRecord {
            id: window_id,
            dataset: dataset.clone(),
            output_dir: output_dir.display().to_string(),
            prior_run_dirs: prior_run_dirs
                .iter()
                .map(|path: &PathBuf| path.display().to_string())
                .collect(),
            command,
            status,
        });
        prior_run_dirs.push(output_dir);
    }

    let report = RollingWorkflowReport {
        dataset_count: config.datasets.len(),
        output_root: config.output_root.display().to_string(),
        min_windows: config.walk_forward_min_windows,
        dry_run: config.dry_run,
        windows,
        final_walk_forward_report: prior_run_dirs
            .last()
            .map(|dir| {
                dir.join("walk_forward")
                    .join("walk_forward_report.json")
                    .display()
                    .to_string()
            })
            .unwrap_or_default(),
        final_strategy_handoff_report: prior_run_dirs
            .last()
            .map(|dir| {
                dir.join("walk_forward")
                    .join("event_ml_strategy_handoff.json")
                    .display()
                    .to_string()
            })
            .unwrap_or_default(),
    };
    write_report(&config.output_root, &report)?;

    eprintln!();
    eprintln!("rolling_workflow_status=completed");
    eprintln!(
        "rolling_workflow_output_root={}",
        config.output_root.display()
    );
    if !report.final_walk_forward_report.is_empty() {
        eprintln!(
            "rolling_workflow_final_walk_forward_report={}",
            report.final_walk_forward_report
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Config {
    datasets: Vec<String>,
    output_root: PathBuf,
    entry_secs: i64,
    tolerance_secs: i64,
    top_n: usize,
    min_edge: f64,
    features: Option<String>,
    search_l2: String,
    search_min_edge: String,
    search_learning_rate: String,
    search_epochs: usize,
    walk_forward_min_windows: usize,
    walk_forward_min_test_trades: usize,
    required_strategy_profile: String,
    runtime_score: Option<String>,
    replay_parity_ready: bool,
    dry_run: bool,
}

impl Config {
    fn parse(args: &[String]) -> Result<Self> {
        let datasets = parse_datasets(args)?;
        ensure_unique_datasets(&datasets)?;
        let output_root = flag_value(args, "--output-root")
            .map(PathBuf::from)
            .unwrap_or_else(default_output_root);
        let entry_secs = parse_i64_flag(args, "--entry-secs", 60)?;
        let tolerance_secs = parse_i64_flag(args, "--tolerance-secs", 30)?;
        let top_n = parse_usize_flag(args, "--top-n", 20)?;
        let min_edge = parse_f64_flag(args, "--min-edge", 0.0)?;
        let features = flag_value(args, "--features");
        let search_l2 =
            flag_value(args, "--search-l2").unwrap_or_else(|| "0,0.001,0.01".to_string());
        let search_min_edge =
            flag_value(args, "--search-min-edge").unwrap_or_else(|| "0,0.02,0.05".to_string());
        let search_learning_rate =
            flag_value(args, "--search-learning-rate").unwrap_or_else(|| "0.03,0.05".to_string());
        let search_epochs = parse_usize_flag(args, "--search-epochs", 500)?;
        let walk_forward_min_windows = parse_usize_flag(args, "--walk-forward-min-windows", 3)?;
        let walk_forward_min_test_trades =
            parse_usize_flag(args, "--walk-forward-min-test-trades", 1)?;
        let required_strategy_profile = flag_value(args, "--required-strategy-profile")
            .unwrap_or_else(|| "event_ml_supervised_tabular".to_string());
        let runtime_score = flag_value(args, "--runtime-score");
        let replay_parity_ready = has_flag(args, "--replay-parity-ready");
        let dry_run = has_flag(args, "--dry-run");

        if entry_secs < 0 {
            bail!("--entry-secs must be non-negative");
        }
        if tolerance_secs < 0 {
            bail!("--tolerance-secs must be non-negative");
        }
        if top_n == 0 {
            bail!("--top-n must be positive");
        }
        if walk_forward_min_windows == 0 {
            bail!("--walk-forward-min-windows must be positive");
        }
        if required_strategy_profile.trim().is_empty() {
            bail!("--required-strategy-profile must not be empty");
        }

        Ok(Config {
            datasets,
            output_root,
            entry_secs,
            tolerance_secs,
            top_n,
            min_edge,
            features,
            search_l2,
            search_min_edge,
            search_learning_rate,
            search_epochs,
            walk_forward_min_windows,
            walk_forward_min_test_trades,
            required_strategy_profile,
            runtime_score,
            replay_parity_ready,
            dry_run,
        })
    }
}

#[derive(Debug, Serialize)]
struct RollingWorkflowReport {
    dataset_count: usize,
    output_root: String,
    min_windows: usize,
    dry_run: bool,
    windows: Vec<RollingWindowRecord>,
    final_walk_forward_report: String,
    final_strategy_handoff_report: String,
}

#[derive(Debug, Serialize)]
struct RollingWindowRecord {
    id: usize,
    dataset: String,
    output_dir: String,
    prior_run_dirs: Vec<String>,
    command: String,
    status: String,
}

fn parse_datasets(args: &[String]) -> Result<Vec<String>> {
    let mut datasets = flag_values(args, "--dataset");
    if let Some(raw) = flag_value(args, "--datasets") {
        datasets.extend(
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned),
        );
    }
    if datasets.is_empty() {
        bail!("at least one --dataset or --datasets entry is required");
    }
    Ok(datasets)
}

fn ensure_unique_datasets(datasets: &[String]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for dataset in datasets {
        if !seen.insert(dataset) {
            bail!("duplicate dataset path: {dataset}");
        }
    }
    Ok(())
}

fn event_ml_workflow_args(
    config: &Config,
    dataset: &str,
    output_dir: &Path,
    prior_run_dirs: &[PathBuf],
) -> Vec<String> {
    let mut args = vec![
        "--dataset".to_string(),
        dataset.to_string(),
        "--output-dir".to_string(),
        output_dir.display().to_string(),
        "--entry-secs".to_string(),
        config.entry_secs.to_string(),
        "--tolerance-secs".to_string(),
        config.tolerance_secs.to_string(),
        "--top-n".to_string(),
        config.top_n.to_string(),
        "--min-edge".to_string(),
        config.min_edge.to_string(),
        "--search-l2".to_string(),
        config.search_l2.clone(),
        "--search-min-edge".to_string(),
        config.search_min_edge.clone(),
        "--search-learning-rate".to_string(),
        config.search_learning_rate.clone(),
        "--search-epochs".to_string(),
        config.search_epochs.to_string(),
        "--walk-forward-min-windows".to_string(),
        config.walk_forward_min_windows.to_string(),
        "--walk-forward-min-test-trades".to_string(),
        config.walk_forward_min_test_trades.to_string(),
    ];
    if let Some(features) = &config.features {
        args.push("--features".to_string());
        args.push(features.clone());
    }
    args.push("--required-strategy-profile".to_string());
    args.push(config.required_strategy_profile.clone());
    if let Some(runtime_score) = &config.runtime_score {
        args.push("--runtime-score".to_string());
        args.push(runtime_score.clone());
    }
    if config.replay_parity_ready {
        args.push("--replay-parity-ready".to_string());
    }
    for run_dir in prior_run_dirs {
        args.push("--walk-forward-run-dir".to_string());
        args.push(run_dir.display().to_string());
    }
    if config.dry_run {
        args.push("--dry-run".to_string());
    }
    args
}

fn write_report(output_root: &Path, report: &RollingWorkflowReport) -> Result<()> {
    let json_path = output_root.join("rolling_workflow_report.json");
    let markdown_path = output_root.join("rolling_workflow_report.md");
    let file =
        File::create(&json_path).with_context(|| format!("create {}", json_path.display()))?;
    serde_json::to_writer_pretty(file, report)
        .with_context(|| format!("write {}", json_path.display()))?;

    let mut markdown = String::new();
    markdown.push_str("# Event ML Rolling Workflow Report\n\n");
    markdown.push_str(&format!("- output_root: `{}`\n", report.output_root));
    markdown.push_str(&format!("- dataset_count: `{}`\n", report.dataset_count));
    markdown.push_str(&format!("- min_windows: `{}`\n", report.min_windows));
    markdown.push_str(&format!("- dry_run: `{}`\n", report.dry_run));
    if !report.final_walk_forward_report.is_empty() {
        markdown.push_str(&format!(
            "- final_walk_forward_report: `{}`\n",
            report.final_walk_forward_report
        ));
    }
    if !report.final_strategy_handoff_report.is_empty() {
        markdown.push_str(&format!(
            "- final_strategy_handoff_report: `{}`\n",
            report.final_strategy_handoff_report
        ));
    }
    markdown.push_str("\n## Windows\n\n");
    markdown.push_str("| id | status | dataset | prior_runs | output_dir |\n");
    markdown.push_str("| ---: | --- | --- | ---: | --- |\n");
    for window in &report.windows {
        markdown.push_str(&format!(
            "| {} | `{}` | `{}` | {} | `{}` |\n",
            window.id,
            window.status,
            window.dataset,
            window.prior_run_dirs.len(),
            window.output_dir
        ));
    }
    fs::write(&markdown_path, markdown)
        .with_context(|| format!("write {}", markdown_path.display()))?;

    eprintln!("artifact_rolling_workflow_report={}", json_path.display());
    eprintln!(
        "artifact_rolling_workflow_report_md={}",
        markdown_path.display()
    );
    Ok(())
}

fn ensure_success(window_id: usize, status: ExitStatus) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("rolling window {window_id} failed with status {status}");
    }
}

fn event_ml_workflow_command(args: &[String]) -> Command {
    if let Some(binary) = sibling_example_binary("event_ml_workflow") {
        let mut command = Command::new(binary);
        command.args(args);
        command
    } else {
        let mut command = Command::new("cargo");
        command.args([
            "run",
            "-p",
            "ploy-research",
            "--example",
            "event_ml_workflow",
            "--features",
            "polars-export",
            "--",
        ]);
        command.args(args);
        command
    }
}

fn event_ml_workflow_command_string(args: &[String]) -> String {
    if let Some(binary) = sibling_example_binary("event_ml_workflow") {
        let mut parts = vec![shell_quote(&binary.display().to_string())];
        parts.extend(args.iter().map(|arg| shell_quote(arg)));
        parts.join(" ")
    } else {
        shell_command("event_ml_workflow", args, true)
    }
}

fn sibling_example_binary(example: &str) -> Option<PathBuf> {
    let current = env::current_exe().ok()?;
    let dir = current.parent()?;
    let binary_name = if cfg!(windows) {
        format!("{example}.exe")
    } else {
        example.to_string()
    };
    let candidate = dir.join(binary_name);
    candidate.is_file().then_some(candidate)
}

fn default_output_root() -> PathBuf {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    PathBuf::from("/tmp").join(format!("ploy-event-ml-rolling-{seconds}"))
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn flag_values(args: &[String], flag: &str) -> Vec<String> {
    args.windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .collect()
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn parse_i64_flag(args: &[String], flag: &str, default: i64) -> Result<i64> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<i64>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_usize_flag(args: &[String], flag: &str, default: usize) -> Result<usize> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<usize>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_f64_flag(args: &[String], flag: &str, default: f64) -> Result<f64> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<f64>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn shell_command(example: &str, args: &[String], polars_export: bool) -> String {
    let mut parts = vec![
        "cargo".to_string(),
        "run".to_string(),
        "-p".to_string(),
        "ploy-research".to_string(),
        "--example".to_string(),
        example.to_string(),
    ];
    if polars_export {
        parts.push("--features".to_string());
        parts.push("polars-export".to_string());
    }
    parts.push("--".to_string());
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_./:=,".contains(ch))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn print_usage() {
    println!(
        r#"Event ML rolling workflow runner

Usage:
  cargo run -p ploy-research --example event_ml_rolling_workflow --features polars-export -- \
    --dataset <event-root-dir-1> --dataset <event-root-dir-2> --dataset <event-root-dir-3>

Options:
  --dataset <dir>                    Event-root dataset directory. Repeat for rolling windows.
  --datasets <csv>                   Additional comma-separated event-root dataset directories.
  --output-root <dir>                Root directory for per-window workflow runs.
  --entry-secs <secs>                Entry row target before settlement. Default: 60.
  --tolerance-secs <secs>            Entry row tolerance. Default: 30.
  --top-n <n>                        Attribution rows and whitelist size. Default: 20.
  --min-edge <value>                 Baseline trading edge threshold. Default: 0.0.
  --features <csv>                   Override default feature list.
  --search-l2 <csv>                  Logistic L2 values. Default: 0,0.001,0.01.
  --search-min-edge <csv>            Min-edge values. Default: 0,0.02,0.05.
  --search-learning-rate <csv>       Learning rates. Default: 0.03,0.05.
  --search-epochs <n>                Epochs for candidates. Default: 500.
  --walk-forward-min-windows <n>     Minimum windows required before DL/RL readiness. Default: 3.
  --walk-forward-min-test-trades <n> Minimum test trades per window. Default: 1.
  --required-strategy-profile <id>   Dry-run handoff profile. Default: event_ml_supervised_tabular.
  --runtime-score <id>               Runtime scorer identifier. Required for ready handoff.
  --replay-parity-ready              Mark recorded replay/runtime parity as passed for handoff.
  --dry-run                          Print and record commands without running windows.
  -h, --help                         Show this help.
"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        std::iter::once("event_ml_rolling_workflow".to_string())
            .chain(items.iter().map(|item| (*item).to_string()))
            .collect()
    }

    #[test]
    fn parses_repeated_and_csv_datasets() {
        let config = Config::parse(&args(&[
            "--dataset",
            "/tmp/window-1",
            "--datasets",
            "/tmp/window-2,/tmp/window-3",
            "--output-root",
            "/tmp/rolling",
            "--dry-run",
        ]))
        .unwrap();

        assert_eq!(
            config.datasets,
            vec!["/tmp/window-1", "/tmp/window-2", "/tmp/window-3"]
        );
        assert_eq!(config.output_root, PathBuf::from("/tmp/rolling"));
        assert!(config.dry_run);
    }

    #[test]
    fn rejects_duplicate_datasets() {
        let error = Config::parse(&args(&[
            "--dataset",
            "/tmp/window-1",
            "--datasets",
            "/tmp/window-1",
        ]))
        .unwrap_err()
        .to_string();

        assert!(error.contains("duplicate dataset path"));
    }

    #[test]
    fn current_window_receives_prior_run_dirs() {
        let config = Config::parse(&args(&[
            "--dataset",
            "/tmp/window-1",
            "--dataset",
            "/tmp/window-2",
            "--output-root",
            "/tmp/rolling",
            "--search-l2",
            "0",
            "--runtime-score",
            "event_ml_model:baseline_v1",
            "--replay-parity-ready",
        ]))
        .unwrap();
        let prior = vec![PathBuf::from("/tmp/rolling/window_001_event_ml")];
        let args = event_ml_workflow_args(
            &config,
            "/tmp/window-2",
            Path::new("/tmp/rolling/window_002_event_ml"),
            &prior,
        );

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--walk-forward-run-dir", "/tmp/rolling/window_001_event_ml"]));
        assert!(args.windows(2).any(|pair| pair == ["--search-l2", "0"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--runtime-score", "event_ml_model:baseline_v1"]));
        assert!(args.iter().any(|arg| arg == "--replay-parity-ready"));
    }
}
