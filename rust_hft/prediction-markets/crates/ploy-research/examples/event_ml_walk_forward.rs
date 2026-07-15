use std::env;
use std::fs::{self, File};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use ploy_research::{
    build_event_ml_strategy_handoff, build_walk_forward_report, event_ml_strategy_handoff_markdown,
    walk_forward_report_markdown, EventMlStrategyHandoffConfig, WalkForwardConfig,
};

fn main() -> Result<()> {
    let config = Config::parse(env::args().skip(1))?;
    let output_dir = config.output_dir.clone().unwrap_or_else(|| {
        config
            .run_dirs
            .first()
            .expect("run_dirs is validated")
            .join("walk_forward")
    });
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("create output dir {}", output_dir.display()))?;

    let report = build_walk_forward_report(&WalkForwardConfig {
        run_dirs: config.run_dirs,
        min_windows: config.min_windows,
        min_test_trades_per_window: config.min_test_trades_per_window,
    })?;
    let markdown = walk_forward_report_markdown(&report);

    let json_path = output_dir.join("walk_forward_report.json");
    let markdown_path = output_dir.join("walk_forward_report.md");
    let json_file =
        File::create(&json_path).with_context(|| format!("create {}", json_path.display()))?;
    serde_json::to_writer_pretty(json_file, &report)
        .with_context(|| format!("write {}", json_path.display()))?;
    fs::write(&markdown_path, markdown)
        .with_context(|| format!("write {}", markdown_path.display()))?;

    let handoff = build_event_ml_strategy_handoff(
        &report,
        &EventMlStrategyHandoffConfig {
            required_strategy_profile: config.required_strategy_profile.clone(),
            runtime_score: config.runtime_score.clone(),
            replay_parity_ready: config.replay_parity_ready,
            source_report_path: Some(json_path.display().to_string()),
        },
    );
    let handoff_json_path = output_dir.join("event_ml_strategy_handoff.json");
    let handoff_md_path = output_dir.join("event_ml_strategy_handoff.md");
    let handoff_file = File::create(&handoff_json_path)
        .with_context(|| format!("create {}", handoff_json_path.display()))?;
    serde_json::to_writer_pretty(handoff_file, &handoff)
        .with_context(|| format!("write {}", handoff_json_path.display()))?;
    fs::write(
        &handoff_md_path,
        event_ml_strategy_handoff_markdown(&handoff),
    )
    .with_context(|| format!("write {}", handoff_md_path.display()))?;

    eprintln!("artifact_walk_forward_report={}", json_path.display());
    eprintln!(
        "artifact_walk_forward_report_md={}",
        markdown_path.display()
    );
    eprintln!("walk_forward_status={}", report.readiness.status.as_str());
    eprintln!(
        "walk_forward_ready_for_dl_rl={}",
        report.readiness.ready_for_dl_rl
    );
    eprintln!(
        "artifact_event_ml_strategy_handoff={}",
        handoff_json_path.display()
    );
    eprintln!(
        "event_ml_strategy_handoff_status={}",
        handoff.status.as_str()
    );
    if !handoff.blocked_gate_ids.is_empty() {
        eprintln!(
            "event_ml_strategy_handoff_blockers={}",
            handoff.blocked_gate_ids.join(",")
        );
    }
    if !report.readiness.missing_gate_ids.is_empty() {
        eprintln!(
            "walk_forward_missing_gates={}",
            report.readiness.missing_gate_ids.join(",")
        );
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Config {
    run_dirs: Vec<PathBuf>,
    output_dir: Option<PathBuf>,
    min_windows: usize,
    min_test_trades_per_window: usize,
    required_strategy_profile: String,
    runtime_score: Option<String>,
    replay_parity_ready: bool,
}

impl Config {
    fn parse<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let args = args.into_iter().collect::<Vec<_>>();
        if has_flag(&args, "--help") || has_flag(&args, "-h") {
            print_help();
            std::process::exit(0);
        }

        let mut run_dirs = flag_values(&args, "--run-dir")
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        if let Some(csv) = flag_value(&args, "--run-dirs") {
            run_dirs.extend(
                csv.split(',')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(PathBuf::from),
            );
        }
        if run_dirs.is_empty() {
            bail!("--run-dir <workflow-run-dir> is required");
        }

        let output_dir = flag_value(&args, "--output-dir").map(PathBuf::from);
        let min_windows = parse_usize_flag(&args, "--min-windows", 3)?;
        let min_test_trades_per_window =
            parse_usize_flag(&args, "--min-test-trades-per-window", 1)?;
        let required_strategy_profile = flag_value(&args, "--required-strategy-profile")
            .unwrap_or_else(|| "event_ml_supervised_tabular".to_string());
        let runtime_score = flag_value(&args, "--runtime-score");
        let replay_parity_ready = has_flag(&args, "--replay-parity-ready");
        if min_windows == 0 {
            bail!("--min-windows must be positive");
        }
        if required_strategy_profile.trim().is_empty() {
            bail!("--required-strategy-profile must not be empty");
        }

        Ok(Config {
            run_dirs,
            output_dir,
            min_windows,
            min_test_trades_per_window,
            required_strategy_profile,
            runtime_score,
            replay_parity_ready,
        })
    }
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

fn parse_usize_flag(args: &[String], flag: &str, default: usize) -> Result<usize> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<usize>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn print_help() {
    println!(
        r#"Event ML walk-forward gate artifact writer

Usage:
  cargo run -p ploy-research --example event_ml_walk_forward -- \
    --run-dir <workflow-run-dir> [--run-dir <workflow-run-dir> ...]

Options:
  --run-dir <dir>                    Workflow run directory containing workflow_report.json
                                     and hyperparameter/hyperparameter_search.json.
  --run-dirs <csv>                   Additional comma-separated workflow run directories.
  --output-dir <dir>                 Output artifact directory. Default: <first-run-dir>/walk_forward.
  --min-windows <n>                  Minimum windows required for DL/RL readiness. Default: 3.
  --min-test-trades-per-window <n>   Minimum test trades per window. Default: 1.
  --required-strategy-profile <id>   Dry-run handoff profile. Default: event_ml_supervised_tabular.
  --runtime-score <id>               Runtime scorer identifier. Required for a ready handoff.
  --replay-parity-ready              Mark replay/runtime parity as ready for dry-run handoff.
  -h, --help                         Show this help.
"#
    );
}
