use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Coverage,
    Attribution,
    Baseline,
    Hyperparameter,
    WalkForward,
}

impl Phase {
    fn example(self) -> Option<&'static str> {
        match self {
            Phase::Coverage => Some("event_dataset_coverage"),
            Phase::Attribution => Some("event_factor_attribution"),
            Phase::Baseline => Some("event_dataset_baseline"),
            Phase::Hyperparameter => None,
            Phase::WalkForward => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Phase::Coverage => "coverage diagnostics",
            Phase::Attribution => "AutoML-style factor attribution",
            Phase::Baseline => "fixed supervised baseline",
            Phase::Hyperparameter => "bounded logistic hyperparameter search",
            Phase::WalkForward => "walk-forward executable-price gate",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Phase::Coverage => "coverage",
            Phase::Attribution => "attribution",
            Phase::Baseline => "baseline",
            Phase::Hyperparameter => "hyperparameter",
            Phase::WalkForward => "walk_forward",
        })
    }
}

#[derive(Debug, Clone)]
struct Config {
    dataset: String,
    entry_secs: i64,
    tolerance_secs: i64,
    top_n: usize,
    min_edge: f64,
    features: Option<String>,
    output_dir: Option<PathBuf>,
    search_l2: Vec<f64>,
    search_min_edge: Vec<f64>,
    search_learning_rate: Vec<f64>,
    search_epochs: usize,
    walk_forward_min_windows: usize,
    walk_forward_min_test_trades: usize,
    walk_forward_extra_run_dirs: Vec<PathBuf>,
    required_strategy_profile: String,
    runtime_score: Option<String>,
    replay_parity_ready: bool,
    phases: Vec<Phase>,
    dry_run: bool,
}

#[derive(Debug)]
struct WorkflowState {
    run_dir: PathBuf,
    governed_features: Option<String>,
    records: Vec<PhaseRecord>,
}

#[derive(Debug, Serialize)]
struct WorkflowReport<'a> {
    dataset: &'a str,
    run_dir: String,
    entry_secs: i64,
    tolerance_secs: i64,
    phases: Vec<String>,
    feature_source: &'a str,
    governed_features: Vec<&'a str>,
    records: &'a [PhaseRecord],
    next_gates: &'a [&'a str],
}

#[derive(Debug, Serialize)]
struct PhaseRecord {
    phase: String,
    label: &'static str,
    command: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct HyperparameterSearchReport {
    selection_rule: &'static str,
    candidates: Vec<CandidateRecord>,
    best_candidate: Option<CandidateRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct CandidateRecord {
    id: usize,
    l2: f64,
    min_edge: f64,
    learning_rate: f64,
    epochs: usize,
    output_json: String,
    val_pnl: f64,
    val_logloss: f64,
    val_auc: Option<f64>,
    val_trades: usize,
    test_pnl: f64,
    test_logloss: f64,
    test_auc: Option<f64>,
    test_trades: usize,
}

#[derive(Debug, Deserialize)]
struct BaselineArtifact {
    metrics: Vec<BaselineMetric>,
}

#[derive(Debug, Deserialize)]
struct BaselineMetric {
    split: String,
    logloss: f64,
    auc: Option<f64>,
    trades: usize,
    pnl: f64,
}

#[derive(Debug, Deserialize)]
struct WalkForwardGateArtifact {
    readiness: WalkForwardReadinessArtifact,
}

#[derive(Debug, Deserialize)]
struct WalkForwardReadinessArtifact {
    ready_for_dl_rl: bool,
    status: String,
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if has_flag(&args, "--help") || has_flag(&args, "-h") {
        print_usage();
        return Ok(());
    }

    let config = parse_config(&args)?;
    let mut state = WorkflowState {
        run_dir: workflow_run_dir(&config)?,
        governed_features: config.features.clone(),
        records: Vec::new(),
    };
    fs::create_dir_all(&state.run_dir)
        .with_context(|| format!("create workflow run dir {}", state.run_dir.display()))?;
    print_workflow_header(&config, &state);

    for phase in &config.phases {
        run_phase(*phase, &config, &mut state)?;
    }
    write_workflow_report(&config, &state)?;

    eprintln!();
    eprintln!("workflow_status=completed");
    eprintln!("workflow_run_dir={}", state.run_dir.display());
    eprintln!("next_gates=walk_forward_backtest -> DL/RL only if justified -> dry_run_handoff");
    Ok(())
}

fn parse_config(args: &[String]) -> Result<Config> {
    let dataset = flag_value(args, "--dataset")
        .or_else(|| flag_value(args, "--dataset-root"))
        .context("--dataset <event-root-dataset-dir> is required")?;
    let entry_secs = parse_i64_flag(args, "--entry-secs", 60)?;
    let tolerance_secs = parse_i64_flag(args, "--tolerance-secs", 30)?;
    let top_n = parse_usize_flag(args, "--top-n", 20)?;
    let min_edge = parse_f64_flag(args, "--min-edge", 0.0)?;
    let features = flag_value(args, "--features");
    let output_dir = flag_value(args, "--output-dir").map(PathBuf::from);
    let phases = flag_value(args, "--phases")
        .map(|raw| parse_phases(&raw))
        .transpose()?
        .unwrap_or_else(|| {
            vec![
                Phase::Coverage,
                Phase::Attribution,
                Phase::Baseline,
                Phase::Hyperparameter,
                Phase::WalkForward,
            ]
        });
    let dry_run = has_flag(args, "--dry-run");
    let search_l2 = parse_f64_list_flag(args, "--search-l2", &[0.0, 0.001, 0.01])?;
    let search_min_edge = parse_f64_list_flag(args, "--search-min-edge", &[0.0, 0.02, 0.05])?;
    let search_learning_rate = parse_f64_list_flag(args, "--search-learning-rate", &[0.03, 0.05])?;
    let search_epochs = parse_usize_flag(args, "--search-epochs", 500)?;
    let walk_forward_min_windows = parse_usize_flag(args, "--walk-forward-min-windows", 3)?;
    let walk_forward_min_test_trades = parse_usize_flag(args, "--walk-forward-min-test-trades", 1)?;
    let walk_forward_extra_run_dirs = parse_walk_forward_extra_run_dirs(args);
    let required_strategy_profile = flag_value(args, "--required-strategy-profile")
        .unwrap_or_else(|| "event_ml_supervised_tabular".to_string());
    let runtime_score = flag_value(args, "--runtime-score");
    let replay_parity_ready = has_flag(args, "--replay-parity-ready");

    if entry_secs < 0 {
        bail!("--entry-secs must be non-negative");
    }
    if tolerance_secs < 0 {
        bail!("--tolerance-secs must be non-negative");
    }
    if top_n == 0 {
        bail!("--top-n must be positive");
    }
    if search_l2.iter().any(|value| *value < 0.0) {
        bail!("--search-l2 values must be non-negative");
    }
    if search_min_edge.iter().any(|value| *value < 0.0) {
        bail!("--search-min-edge values must be non-negative");
    }
    if search_learning_rate
        .iter()
        .any(|value| !(0.0..1.0).contains(value))
    {
        bail!("--search-learning-rate values must be in [0, 1)");
    }
    if walk_forward_min_windows == 0 {
        bail!("--walk-forward-min-windows must be positive");
    }
    if required_strategy_profile.trim().is_empty() {
        bail!("--required-strategy-profile must not be empty");
    }

    Ok(Config {
        dataset,
        entry_secs,
        tolerance_secs,
        top_n,
        min_edge,
        features,
        output_dir,
        search_l2,
        search_min_edge,
        search_learning_rate,
        search_epochs,
        walk_forward_min_windows,
        walk_forward_min_test_trades,
        walk_forward_extra_run_dirs,
        required_strategy_profile,
        runtime_score,
        replay_parity_ready,
        phases,
        dry_run,
    })
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

fn parse_f64_list_flag(args: &[String], flag: &str, default: &[f64]) -> Result<Vec<f64>> {
    flag_value(args, flag)
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| {
                    item.parse::<f64>()
                        .with_context(|| format!("invalid {flag}: {item}"))
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()
        .map(|value| value.unwrap_or_else(|| default.to_vec()))
}

fn parse_walk_forward_extra_run_dirs(args: &[String]) -> Vec<PathBuf> {
    let mut run_dirs = flag_values(args, "--walk-forward-run-dir")
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if let Some(raw) = flag_value(args, "--walk-forward-run-dirs") {
        run_dirs.extend(
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(PathBuf::from),
        );
    }
    run_dirs
}

fn parse_phases(raw: &str) -> Result<Vec<Phase>> {
    let mut phases = Vec::new();
    for item in raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let phase = match item {
            "coverage" => Phase::Coverage,
            "attribution" | "factor-attribution" | "automl" => Phase::Attribution,
            "baseline" | "fixed-baseline" => Phase::Baseline,
            "hyperparameter" | "hyperparameter-search" | "search" => Phase::Hyperparameter,
            "walk-forward" | "walk_forward" | "backtest" | "walk-forward-backtest" => {
                Phase::WalkForward
            }
            other => bail!("unknown workflow phase: {other}"),
        };
        phases.push(phase);
    }
    if phases.is_empty() {
        bail!("--phases must include at least one phase");
    }
    Ok(phases)
}

fn workflow_run_dir(config: &Config) -> Result<PathBuf> {
    if let Some(output_dir) = &config.output_dir {
        return Ok(output_dir.clone());
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_secs();
    Ok(PathBuf::from(&config.dataset)
        .join("workflow_runs")
        .join(format!("event_ml_{seconds}")))
}

fn print_workflow_header(config: &Config, state: &WorkflowState) {
    eprintln!("=== Event ML AutoML Workflow ===");
    eprintln!("dataset={}", config.dataset);
    eprintln!("run_dir={}", state.run_dir.display());
    eprintln!(
        "entry_secs={} tolerance_secs={} phases={} dry_run={}",
        config.entry_secs,
        config.tolerance_secs,
        format_phases(&config.phases),
        config.dry_run
    );
    eprintln!(
        "walk_forward_extra_run_dirs={}",
        config.walk_forward_extra_run_dirs.len()
    );
    eprintln!(
        "order=coverage -> AutoML factor attribution -> governed feature set -> fixed baseline -> bounded hyperparameter search -> walk-forward/backtest -> DL/RL gates"
    );
}

fn format_phases(phases: &[Phase]) -> String {
    phases
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn run_phase(phase: Phase, config: &Config, state: &mut WorkflowState) -> Result<()> {
    if phase == Phase::Hyperparameter {
        return run_hyperparameter_phase(config, state);
    }
    if phase == Phase::WalkForward {
        return run_walk_forward_phase(config, state);
    }

    let args = phase_args(phase, config, state);
    let example = phase
        .example()
        .expect("non-hyperparameter phase must have an example");
    let command = shell_command(example, &args);
    eprintln!();
    eprintln!("--- phase={} label=\"{}\" ---", phase, phase.label());
    eprintln!("command={command}");

    if config.dry_run {
        state.records.push(PhaseRecord {
            phase: phase.to_string(),
            label: phase.label(),
            command,
            status: "dry_run".to_string(),
        });
        return Ok(());
    }

    let status = Command::new("cargo")
        .args(["run", "-p", "ploy-research", "--example", example])
        .args(["--features", "polars-export", "--"])
        .args(&args)
        .status()
        .with_context(|| format!("spawn phase {phase}"))?;
    ensure_success(phase, status)?;
    if phase == Phase::Attribution && config.features.is_none() {
        let whitelist = read_feature_whitelist(&attribution_output_dir(state))?;
        if !whitelist.is_empty() {
            let features = whitelist.join(",");
            eprintln!(
                "governed_feature_source={} governed_feature_count={}",
                attribution_output_dir(state)
                    .join("feature_whitelist.txt")
                    .display(),
                whitelist.len()
            );
            state.governed_features = Some(features);
        }
    }
    state.records.push(PhaseRecord {
        phase: phase.to_string(),
        label: phase.label(),
        command,
        status: "passed".to_string(),
    });
    Ok(())
}

fn run_walk_forward_phase(config: &Config, state: &mut WorkflowState) -> Result<()> {
    let workflow_report = state.run_dir.join("workflow_report.json");
    if !workflow_report.exists() && !config.dry_run {
        write_workflow_report(config, state)?;
    }
    let output_dir = state.run_dir.join("walk_forward");
    let args = walk_forward_phase_args(config, state, &output_dir);
    let command = shell_command_without_features("event_ml_walk_forward", &args);
    eprintln!();
    eprintln!(
        "--- phase={} label=\"{}\" ---",
        Phase::WalkForward,
        Phase::WalkForward.label()
    );
    eprintln!("command={command}");

    if config.dry_run {
        state.records.push(PhaseRecord {
            phase: Phase::WalkForward.to_string(),
            label: Phase::WalkForward.label(),
            command,
            status: "dry_run".to_string(),
        });
        return Ok(());
    }

    let status = Command::new("cargo")
        .args([
            "run",
            "-p",
            "ploy-research",
            "--example",
            "event_ml_walk_forward",
            "--",
        ])
        .args(&args)
        .status()
        .context("spawn walk-forward gate phase")?;
    if !status.success() {
        bail!("walk-forward gate failed with status {status}");
    }

    let report_path = output_dir.join("walk_forward_report.json");
    let report_file =
        File::open(&report_path).with_context(|| format!("open {}", report_path.display()))?;
    let report: WalkForwardGateArtifact = serde_json::from_reader(report_file)
        .with_context(|| format!("parse {}", report_path.display()))?;
    eprintln!(
        "walk_forward_readiness={} ready_for_dl_rl={}",
        report.readiness.status, report.readiness.ready_for_dl_rl
    );
    state.records.push(PhaseRecord {
        phase: Phase::WalkForward.to_string(),
        label: Phase::WalkForward.label(),
        command,
        status: report.readiness.status,
    });
    Ok(())
}

fn walk_forward_phase_args(
    config: &Config,
    state: &WorkflowState,
    output_dir: &Path,
) -> Vec<String> {
    let mut args = vec![
        "--run-dir".to_string(),
        state.run_dir.display().to_string(),
        "--output-dir".to_string(),
        output_dir.display().to_string(),
        "--min-windows".to_string(),
        config.walk_forward_min_windows.to_string(),
        "--min-test-trades-per-window".to_string(),
        config.walk_forward_min_test_trades.to_string(),
    ];
    for run_dir in &config.walk_forward_extra_run_dirs {
        args.push("--run-dir".to_string());
        args.push(run_dir.display().to_string());
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
    args
}

fn run_hyperparameter_phase(config: &Config, state: &mut WorkflowState) -> Result<()> {
    let command = format!(
        "bounded logistic search l2={} min_edge={} learning_rate={} epochs={}",
        format_f64_list(&config.search_l2),
        format_f64_list(&config.search_min_edge),
        format_f64_list(&config.search_learning_rate),
        config.search_epochs
    );
    eprintln!();
    eprintln!(
        "--- phase={} label=\"{}\" ---",
        Phase::Hyperparameter,
        Phase::Hyperparameter.label()
    );
    eprintln!("command={command}");

    if config.dry_run {
        state.records.push(PhaseRecord {
            phase: Phase::Hyperparameter.to_string(),
            label: Phase::Hyperparameter.label(),
            command,
            status: "dry_run".to_string(),
        });
        return Ok(());
    }

    let features = state.governed_features.as_deref().context(
        "hyperparameter phase requires governed features from attribution or --features",
    )?;
    let output_dir = state.run_dir.join("hyperparameter");
    fs::create_dir_all(&output_dir).with_context(|| format!("create {}", output_dir.display()))?;

    let mut candidates = Vec::new();
    let mut candidate_id = 0usize;
    for l2 in &config.search_l2 {
        for min_edge in &config.search_min_edge {
            for learning_rate in &config.search_learning_rate {
                candidate_id += 1;
                let candidate_dir = output_dir.join(format!("candidate_{candidate_id:03}"));
                let output_json = candidate_dir.join("baseline_metrics.json");
                let args = baseline_candidate_args(
                    config,
                    features,
                    *l2,
                    *min_edge,
                    *learning_rate,
                    config.search_epochs,
                    &output_json,
                );
                let candidate_command = shell_command("event_dataset_baseline", &args);
                eprintln!("candidate_id={candidate_id} command={candidate_command}");
                let status = Command::new("cargo")
                    .args([
                        "run",
                        "-p",
                        "ploy-research",
                        "--example",
                        "event_dataset_baseline",
                    ])
                    .args(["--features", "polars-export", "--"])
                    .args(&args)
                    .status()
                    .with_context(|| format!("spawn hyperparameter candidate {candidate_id}"))?;
                if !status.success() {
                    bail!("hyperparameter candidate {candidate_id} failed with status {status}");
                }
                candidates.push(read_candidate_record(
                    candidate_id,
                    *l2,
                    *min_edge,
                    *learning_rate,
                    config.search_epochs,
                    &output_json,
                )?);
            }
        }
    }

    let best_candidate = candidates.iter().cloned().max_by(compare_candidates);
    write_hyperparameter_report(&output_dir, &candidates, best_candidate.clone())?;
    if let Some(best) = &best_candidate {
        eprintln!(
            "hyperparameter_best id={} val_pnl={:.4} val_logloss={:.4} test_pnl={:.4}",
            best.id, best.val_pnl, best.val_logloss, best.test_pnl
        );
    }
    state.records.push(PhaseRecord {
        phase: Phase::Hyperparameter.to_string(),
        label: Phase::Hyperparameter.label(),
        command,
        status: "passed".to_string(),
    });
    Ok(())
}

fn format_f64_list(values: &[f64]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn baseline_candidate_args(
    config: &Config,
    features: &str,
    l2: f64,
    min_edge: f64,
    learning_rate: f64,
    epochs: usize,
    output_json: &Path,
) -> Vec<String> {
    vec![
        "--dataset".to_string(),
        config.dataset.clone(),
        "--entry-secs".to_string(),
        config.entry_secs.to_string(),
        "--tolerance-secs".to_string(),
        config.tolerance_secs.to_string(),
        "--min-edge".to_string(),
        min_edge.to_string(),
        "--epochs".to_string(),
        epochs.to_string(),
        "--learning-rate".to_string(),
        learning_rate.to_string(),
        "--l2".to_string(),
        l2.to_string(),
        "--features".to_string(),
        features.to_string(),
        "--output-json".to_string(),
        output_json.display().to_string(),
    ]
}

fn read_candidate_record(
    id: usize,
    l2: f64,
    min_edge: f64,
    learning_rate: f64,
    epochs: usize,
    output_json: &Path,
) -> Result<CandidateRecord> {
    let file =
        File::open(output_json).with_context(|| format!("open {}", output_json.display()))?;
    let artifact: BaselineArtifact = serde_json::from_reader(file)
        .with_context(|| format!("parse {}", output_json.display()))?;
    let val = metric_for_split(&artifact, "val")?;
    let test = metric_for_split(&artifact, "test")?;
    Ok(CandidateRecord {
        id,
        l2,
        min_edge,
        learning_rate,
        epochs,
        output_json: output_json.display().to_string(),
        val_pnl: val.pnl,
        val_logloss: val.logloss,
        val_auc: val.auc,
        val_trades: val.trades,
        test_pnl: test.pnl,
        test_logloss: test.logloss,
        test_auc: test.auc,
        test_trades: test.trades,
    })
}

fn metric_for_split<'a>(artifact: &'a BaselineArtifact, split: &str) -> Result<&'a BaselineMetric> {
    artifact
        .metrics
        .iter()
        .find(|metric| metric.split == split)
        .with_context(|| format!("missing {split} metric in baseline artifact"))
}

fn compare_candidates(left: &CandidateRecord, right: &CandidateRecord) -> std::cmp::Ordering {
    left.val_pnl
        .partial_cmp(&right.val_pnl)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            right
                .val_logloss
                .partial_cmp(&left.val_logloss)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn write_hyperparameter_report(
    output_dir: &Path,
    candidates: &[CandidateRecord],
    best_candidate: Option<CandidateRecord>,
) -> Result<()> {
    let report = HyperparameterSearchReport {
        selection_rule: "maximize validation PnL; break ties by lower validation logloss; test metrics are recorded but not used for selection",
        candidates: candidates.to_vec(),
        best_candidate,
    };
    let json_path = output_dir.join("hyperparameter_search.json");
    let json_file =
        File::create(&json_path).with_context(|| format!("create {}", json_path.display()))?;
    serde_json::to_writer_pretty(json_file, &report)
        .with_context(|| format!("write {}", json_path.display()))?;

    let markdown_path = output_dir.join("hyperparameter_search.md");
    let mut markdown_file = File::create(&markdown_path)
        .with_context(|| format!("create {}", markdown_path.display()))?;
    writeln!(markdown_file, "# Event Hyperparameter Search")?;
    writeln!(markdown_file)?;
    writeln!(
        markdown_file,
        "Selection rule: maximize validation PnL; break ties by lower validation logloss. Test metrics are recorded but not used for selection."
    )?;
    writeln!(markdown_file)?;
    writeln!(
        markdown_file,
        "| id | l2 | min_edge | lr | epochs | val_pnl | val_logloss | val_auc | test_pnl | test_logloss | test_auc |"
    )?;
    writeln!(
        markdown_file,
        "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    )?;
    for candidate in candidates {
        writeln!(
            markdown_file,
            "| {} | {} | {} | {} | {} | {:.4} | {:.4} | {} | {:.4} | {:.4} | {} |",
            candidate.id,
            candidate.l2,
            candidate.min_edge,
            candidate.learning_rate,
            candidate.epochs,
            candidate.val_pnl,
            candidate.val_logloss,
            format_optional(candidate.val_auc),
            candidate.test_pnl,
            candidate.test_logloss,
            format_optional(candidate.test_auc)
        )?;
    }
    if let Some(best) = &report.best_candidate {
        writeln!(markdown_file)?;
        writeln!(
            markdown_file,
            "Best candidate: `{}` with validation PnL `{:.4}` and test PnL `{:.4}`.",
            best.id, best.val_pnl, best.test_pnl
        )?;
    }

    eprintln!("artifact_hyperparameter_search={}", json_path.display());
    Ok(())
}

fn format_optional(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.4}"))
        .unwrap_or_else(|| "null".to_string())
}

fn phase_args(phase: Phase, config: &Config, state: &WorkflowState) -> Vec<String> {
    let mut args = vec![
        "--dataset".to_string(),
        config.dataset.clone(),
        "--entry-secs".to_string(),
        config.entry_secs.to_string(),
    ];

    match phase {
        Phase::Coverage => {
            args.push("--tolerances".to_string());
            args.push(config.tolerance_secs.to_string());
        }
        Phase::Attribution => {
            args.push("--tolerance-secs".to_string());
            args.push(config.tolerance_secs.to_string());
            args.push("--top-n".to_string());
            args.push(config.top_n.to_string());
            args.push("--output-dir".to_string());
            args.push(attribution_output_dir(state).display().to_string());
            args.push("--whitelist-max-features".to_string());
            args.push(config.top_n.to_string());
        }
        Phase::Baseline => {
            args.push("--tolerance-secs".to_string());
            args.push(config.tolerance_secs.to_string());
            args.push("--min-edge".to_string());
            args.push(config.min_edge.to_string());
        }
        Phase::Hyperparameter | Phase::WalkForward => {}
    }

    let features = match phase {
        Phase::Baseline => state.governed_features.as_ref(),
        _ => config.features.as_ref(),
    };
    if let Some(features) = features {
        args.push("--features".to_string());
        args.push(features.clone());
    }

    args
}

fn attribution_output_dir(state: &WorkflowState) -> PathBuf {
    state.run_dir.join("attribution")
}

fn read_feature_whitelist(output_dir: &Path) -> Result<Vec<String>> {
    let path = output_dir.join("feature_whitelist.txt");
    let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn write_workflow_report(config: &Config, state: &WorkflowState) -> Result<()> {
    let governed_features = state
        .governed_features
        .as_deref()
        .map(split_features)
        .unwrap_or_default();
    let next_gates = [
        "walk_forward_backtest",
        "dl_gate_if_justified",
        "rl_gate_if_justified",
        "dry_run_handoff",
    ];
    let report = WorkflowReport {
        dataset: &config.dataset,
        run_dir: state.run_dir.display().to_string(),
        entry_secs: config.entry_secs,
        tolerance_secs: config.tolerance_secs,
        phases: config.phases.iter().map(ToString::to_string).collect(),
        feature_source: if config.features.is_some() {
            "user_supplied"
        } else {
            "automl_attribution_whitelist"
        },
        governed_features,
        records: &state.records,
        next_gates: &next_gates,
    };

    let json_path = state.run_dir.join("workflow_report.json");
    let json_file =
        File::create(&json_path).with_context(|| format!("create {}", json_path.display()))?;
    serde_json::to_writer_pretty(json_file, &report)
        .with_context(|| format!("write {}", json_path.display()))?;

    let markdown_path = state.run_dir.join("workflow_report.md");
    let mut markdown_file = File::create(&markdown_path)
        .with_context(|| format!("create {}", markdown_path.display()))?;
    writeln!(markdown_file, "# Event ML Workflow Report")?;
    writeln!(markdown_file)?;
    writeln!(markdown_file, "- dataset: `{}`", config.dataset)?;
    writeln!(markdown_file, "- run_dir: `{}`", state.run_dir.display())?;
    writeln!(markdown_file, "- entry_secs: `{}`", config.entry_secs)?;
    writeln!(
        markdown_file,
        "- tolerance_secs: `{}`",
        config.tolerance_secs
    )?;
    writeln!(markdown_file)?;
    writeln!(markdown_file, "## Phases")?;
    writeln!(markdown_file)?;
    writeln!(markdown_file, "| phase | status | command |")?;
    writeln!(markdown_file, "| --- | --- | --- |")?;
    for record in &state.records {
        writeln!(
            markdown_file,
            "| `{}` | `{}` | `{}` |",
            record.phase, record.status, record.command
        )?;
    }
    writeln!(markdown_file)?;
    writeln!(markdown_file, "## Governed Features")?;
    writeln!(markdown_file)?;
    for feature in split_features(state.governed_features.as_deref().unwrap_or_default()) {
        writeln!(markdown_file, "- `{feature}`")?;
    }
    writeln!(markdown_file)?;
    writeln!(markdown_file, "## Next Gates")?;
    writeln!(markdown_file)?;
    for gate in next_gates {
        writeln!(markdown_file, "- `{gate}`")?;
    }

    eprintln!("artifact_workflow_report={}", json_path.display());
    Ok(())
}

fn split_features(raw: &str) -> Vec<&str> {
    raw.split(',')
        .map(str::trim)
        .filter(|feature| !feature.is_empty())
        .collect()
}

fn ensure_success(phase: Phase, status: ExitStatus) -> Result<()> {
    if status.success() {
        eprintln!("phase_status={} result=passed", phase);
        Ok(())
    } else {
        bail!("phase {phase} failed with status {status}");
    }
}

fn shell_command(example: &str, args: &[String]) -> String {
    let mut parts = vec![
        "cargo".to_string(),
        "run".to_string(),
        "-p".to_string(),
        "ploy-research".to_string(),
        "--example".to_string(),
        example.to_string(),
        "--features".to_string(),
        "polars-export".to_string(),
        "--".to_string(),
    ];
    parts.extend(args.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

fn shell_command_without_features(example: &str, args: &[String]) -> String {
    let mut parts = vec![
        "cargo".to_string(),
        "run".to_string(),
        "-p".to_string(),
        "ploy-research".to_string(),
        "--example".to_string(),
        example.to_string(),
        "--".to_string(),
    ];
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
        "\
Event ML AutoML workflow runner

Usage:
  cargo run -p ploy-research --example event_ml_workflow --features polars-export -- \\
    --dataset <event-root-dir> [--entry-secs 60] [--tolerance-secs 30]

Options:
  --dataset <dir>          Event-root dataset directory.
  --entry-secs <secs>      Entry row target before settlement. Default: 60.
  --tolerance-secs <secs>  Entry row tolerance. Default: 30.
  --top-n <n>              Attribution rows to print. Default: 20.
  --min-edge <value>       Baseline trading edge threshold. Default: 0.0.
  --features <csv>         Override default feature list.
  --output-dir <dir>       Workflow artifact directory. Default: <dataset>/workflow_runs/event_ml_<timestamp>.
  --phases <csv>           coverage,attribution,baseline,hyperparameter,walk-forward. Default: all five.
  --search-l2 <csv>        Logistic L2 values for bounded search. Default: 0,0.001,0.01.
  --search-min-edge <csv>  Min-edge values for bounded search. Default: 0,0.02,0.05.
  --search-learning-rate <csv>
                           Learning rates for bounded search. Default: 0.03,0.05.
  --search-epochs <n>      Epochs for bounded search candidates. Default: 500.
  --walk-forward-min-windows <n>
                           Minimum windows required before DL/RL readiness. Default: 3.
  --walk-forward-min-test-trades <n>
                           Minimum test trades per walk-forward window. Default: 1.
  --walk-forward-run-dir <dir>
                           Add a completed workflow run dir to the walk-forward gate.
                           Repeat for rolling windows from prior datasets.
  --walk-forward-run-dirs <csv>
                           Add comma-separated completed workflow run dirs.
  --required-strategy-profile <id>
                           Dry-run handoff profile. Default: event_ml_supervised_tabular.
  --runtime-score <id>     Runtime scorer identifier. Required for ready dry-run handoff.
  --replay-parity-ready    Mark recorded replay/runtime parity as passed for handoff.
  --dry-run                Print commands without running phases.
"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        std::iter::once("event_ml_workflow".to_string())
            .chain(items.iter().map(|item| (*item).to_string()))
            .collect()
    }

    #[test]
    fn parses_default_phase_order() {
        let config = parse_config(&args(&["--dataset", "/tmp/events"])).unwrap();

        assert_eq!(
            config.phases,
            vec![
                Phase::Coverage,
                Phase::Attribution,
                Phase::Baseline,
                Phase::Hyperparameter,
                Phase::WalkForward,
            ]
        );
        assert_eq!(config.entry_secs, 60);
        assert_eq!(config.tolerance_secs, 30);
    }

    #[test]
    fn parses_selected_phase_aliases() {
        let config = parse_config(&args(&[
            "--dataset",
            "/tmp/events",
            "--phases",
            "coverage,automl,fixed-baseline,search,walk-forward",
            "--walk-forward-run-dir",
            "/tmp/run-a",
            "--walk-forward-run-dirs",
            "/tmp/run-b,/tmp/run-c",
            "--runtime-score",
            "event_ml_model:baseline_v1",
            "--replay-parity-ready",
            "--dry-run",
        ]))
        .unwrap();

        assert_eq!(
            config.phases,
            vec![
                Phase::Coverage,
                Phase::Attribution,
                Phase::Baseline,
                Phase::Hyperparameter,
                Phase::WalkForward,
            ]
        );
        assert!(config.dry_run);
        assert_eq!(
            config.walk_forward_extra_run_dirs,
            vec![
                PathBuf::from("/tmp/run-a"),
                PathBuf::from("/tmp/run-b"),
                PathBuf::from("/tmp/run-c")
            ]
        );
        assert_eq!(
            config.runtime_score.as_deref(),
            Some("event_ml_model:baseline_v1")
        );
        assert!(config.replay_parity_ready);
    }

    #[test]
    fn coverage_phase_uses_tolerances_flag() {
        let config = parse_config(&args(&[
            "--dataset",
            "/tmp/events",
            "--entry-secs",
            "90",
            "--tolerance-secs",
            "15",
        ]))
        .unwrap();

        let state = WorkflowState {
            run_dir: PathBuf::from("/tmp/run"),
            governed_features: config.features.clone(),
            records: Vec::new(),
        };
        let phase_args = phase_args(Phase::Coverage, &config, &state);

        assert!(phase_args
            .windows(2)
            .any(|pair| pair == ["--tolerances", "15"]));
        assert!(!phase_args
            .windows(2)
            .any(|pair| pair == ["--tolerance-secs", "15"]));
    }

    #[test]
    fn rejects_unknown_phase() {
        let error = parse_config(&args(&["--dataset", "/tmp/events", "--phases", "rl"]))
            .unwrap_err()
            .to_string();

        assert!(error.contains("unknown workflow phase"));
    }

    #[test]
    fn baseline_consumes_governed_features_after_attribution() {
        let config = parse_config(&args(&[
            "--dataset",
            "/tmp/events",
            "--output-dir",
            "/tmp/run",
        ]))
        .unwrap();
        let state = WorkflowState {
            run_dir: PathBuf::from("/tmp/run"),
            governed_features: Some("fair_prob_up,model_edge_up".to_string()),
            records: Vec::new(),
        };

        let phase_args = phase_args(Phase::Baseline, &config, &state);

        assert!(phase_args
            .windows(2)
            .any(|pair| pair == ["--features", "fair_prob_up,model_edge_up"]));
    }

    #[test]
    fn walk_forward_phase_receives_handoff_gates() {
        let config = parse_config(&args(&[
            "--dataset",
            "/tmp/events",
            "--runtime-score",
            "event_ml_model:baseline_v1",
            "--replay-parity-ready",
        ]))
        .unwrap();
        let state = WorkflowState {
            run_dir: PathBuf::from("/tmp/run"),
            governed_features: None,
            records: Vec::new(),
        };

        let args = walk_forward_phase_args(&config, &state, Path::new("/tmp/run/walk_forward"));

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--runtime-score", "event_ml_model:baseline_v1"]));
        assert!(args.iter().any(|arg| arg == "--replay-parity-ready"));
    }
}
