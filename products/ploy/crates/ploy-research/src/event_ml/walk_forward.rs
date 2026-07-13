use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const WALK_FORWARD_REPORT_VERSION: &str = "event-ml-walk-forward.v1";
pub const EVENT_ML_STRATEGY_HANDOFF_VERSION: &str = "event-ml-strategy-handoff.v1";

#[derive(Debug, Clone)]
pub struct WalkForwardConfig {
    pub run_dirs: Vec<PathBuf>,
    pub min_windows: usize,
    pub min_test_trades_per_window: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WalkForwardReport {
    pub version: String,
    pub selection_rule: String,
    pub readiness: WalkForwardReadiness,
    pub gates: Vec<WalkForwardGate>,
    pub aggregate: WalkForwardAggregate,
    pub windows: Vec<WalkForwardWindow>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalkForwardReadiness {
    pub ready_for_dl_rl: bool,
    pub status: WalkForwardGateStatus,
    pub missing_gate_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WalkForwardGateStatus {
    Ready,
    Blocked,
}

impl WalkForwardGateStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            WalkForwardGateStatus::Ready => "ready",
            WalkForwardGateStatus::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalkForwardGate {
    pub id: String,
    pub passed: bool,
    pub evidence: String,
    pub stop_if_missing: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WalkForwardAggregate {
    pub window_count: usize,
    pub total_test_trades: usize,
    pub total_test_cost: f64,
    pub total_test_pnl: f64,
    pub total_test_roi: f64,
    pub weighted_avg_entry: f64,
    pub mean_test_logloss: f64,
    pub mean_test_pnl: f64,
    pub positive_test_windows: usize,
    pub validation_test_direction_agreement: usize,
    pub max_window_drawdown: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WalkForwardWindow {
    pub id: usize,
    pub run_dir: String,
    pub dataset: String,
    pub entry_secs: i64,
    pub tolerance_secs: i64,
    pub candidate_id: usize,
    pub feature_count: usize,
    pub baseline_metrics_path: String,
    pub validation: WalkForwardMetric,
    pub test: WalkForwardMetric,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WalkForwardMetric {
    pub trades: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub pnl: f64,
    pub cost: f64,
    pub roi: f64,
    pub avg_entry: f64,
    pub logloss: f64,
    pub auc: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventMlStrategyHandoffStatus {
    Ready,
    Blocked,
}

impl EventMlStrategyHandoffStatus {
    pub const fn as_str(&self) -> &'static str {
        match self {
            EventMlStrategyHandoffStatus::Ready => "ready",
            EventMlStrategyHandoffStatus::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventMlStrategyHandoffConfig {
    pub required_strategy_profile: String,
    pub runtime_score: Option<String>,
    pub replay_parity_ready: bool,
    pub source_report_path: Option<String>,
}

impl Default for EventMlStrategyHandoffConfig {
    fn default() -> Self {
        Self {
            required_strategy_profile: "event_ml_supervised_tabular".to_string(),
            runtime_score: None,
            replay_parity_ready: false,
            source_report_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventMlStrategyHandoff {
    pub version: String,
    pub status: EventMlStrategyHandoffStatus,
    pub recommended_action: String,
    pub required_strategy_profile: String,
    pub runtime_score: Option<String>,
    pub replay_parity_ready: bool,
    pub blocked_gate_ids: Vec<String>,
    pub promotion_gate: EventMlStrategyPromotionGate,
    pub strategy: Option<EventMlStrategyCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventMlStrategyPromotionGate {
    pub walk_forward_status: WalkForwardGateStatus,
    pub source_report_path: Option<String>,
    pub aggregate: WalkForwardAggregate,
    pub missing_walk_forward_gate_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventMlStrategyCandidate {
    pub strategy_profile: String,
    pub runtime_score: String,
    pub selection_rule: String,
    pub window_count: usize,
    pub test_trades: usize,
    pub test_pnl: f64,
    pub test_roi: f64,
    pub weighted_avg_entry: f64,
    pub max_window_drawdown: f64,
}

#[derive(Debug, Deserialize)]
struct WorkflowReportArtifact {
    dataset: String,
    entry_secs: i64,
    tolerance_secs: i64,
    governed_features: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HyperparameterSearchArtifact {
    selection_rule: String,
    best_candidate: Option<HyperparameterCandidateArtifact>,
}

#[derive(Debug, Deserialize)]
struct HyperparameterCandidateArtifact {
    id: usize,
    output_json: String,
}

#[derive(Debug, Deserialize)]
struct BaselineArtifact {
    metrics: Vec<BaselineMetricArtifact>,
}

#[derive(Debug, Deserialize)]
struct BaselineMetricArtifact {
    split: String,
    logloss: f64,
    auc: Option<f64>,
    trades: usize,
    wins: usize,
    pnl: f64,
    cost: f64,
    roi: f64,
    avg_entry: f64,
}

pub fn build_walk_forward_report(config: &WalkForwardConfig) -> Result<WalkForwardReport> {
    if config.run_dirs.is_empty() {
        bail!("walk-forward requires at least one workflow run dir");
    }
    if config.min_windows == 0 {
        bail!("min_windows must be positive");
    }
    ensure_unique_run_dirs(&config.run_dirs)?;

    let mut windows = Vec::new();
    let mut selection_rules = Vec::new();
    for (index, run_dir) in config.run_dirs.iter().enumerate() {
        let loaded = load_window(index + 1, run_dir)?;
        if !selection_rules.contains(&loaded.selection_rule) {
            selection_rules.push(loaded.selection_rule);
        }
        windows.push(loaded.window);
    }

    let aggregate = aggregate_windows(&windows);
    let gates = readiness_gates(config, &windows, &aggregate);
    let missing_gate_ids = gates
        .iter()
        .filter(|gate| !gate.passed)
        .map(|gate| gate.id.clone())
        .collect::<Vec<_>>();
    let status = if missing_gate_ids.is_empty() {
        WalkForwardGateStatus::Ready
    } else {
        WalkForwardGateStatus::Blocked
    };

    Ok(WalkForwardReport {
        version: WALK_FORWARD_REPORT_VERSION.to_string(),
        selection_rule: selection_rules.join(" | "),
        readiness: WalkForwardReadiness {
            ready_for_dl_rl: status == WalkForwardGateStatus::Ready,
            status,
            missing_gate_ids,
        },
        gates,
        aggregate,
        windows,
        notes: vec![
            "This report gates DL/RL readiness; it is not a live-edge approval.".to_string(),
            "Hyperparameter candidates are assumed to have been selected by validation metrics only."
                .to_string(),
            "Max drawdown is computed across window-level test PnL until trade-level equity curves are available."
                .to_string(),
        ],
    })
}

pub fn walk_forward_report_markdown(report: &WalkForwardReport) -> String {
    let mut out = String::new();
    out.push_str("# Event ML Walk-Forward Gate\n\n");
    out.push_str(&format!("Version: `{}`\n\n", report.version));
    out.push_str(&format!(
        "Readiness: `{}`\n\n",
        report.readiness.status.as_str()
    ));
    if !report.readiness.missing_gate_ids.is_empty() {
        out.push_str("Missing gates:\n\n");
        for gate_id in &report.readiness.missing_gate_ids {
            out.push_str(&format!("- `{gate_id}`\n"));
        }
        out.push('\n');
    }

    out.push_str("## Aggregate\n\n");
    out.push_str(&format!("- windows: `{}`\n", report.aggregate.window_count));
    out.push_str(&format!(
        "- test trades: `{}`\n",
        report.aggregate.total_test_trades
    ));
    out.push_str(&format!(
        "- test PnL: `{:.4}`\n",
        report.aggregate.total_test_pnl
    ));
    out.push_str(&format!(
        "- test ROI: `{:.4}`\n",
        report.aggregate.total_test_roi
    ));
    out.push_str(&format!(
        "- weighted avg entry: `{:.4}`\n",
        report.aggregate.weighted_avg_entry
    ));
    out.push_str(&format!(
        "- max window drawdown: `{:.4}`\n\n",
        report.aggregate.max_window_drawdown
    ));

    out.push_str("## Gates\n\n");
    out.push_str("| gate | passed | evidence |\n");
    out.push_str("| --- | --- | --- |\n");
    for gate in &report.gates {
        out.push_str(&format!(
            "| `{}` | `{}` | {} |\n",
            gate.id, gate.passed, gate.evidence
        ));
    }

    out.push_str("\n## Windows\n\n");
    out.push_str("| id | dataset | candidate | features | val_pnl | test_pnl | test_trades | test_roi | avg_entry |\n");
    out.push_str("| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for window in &report.windows {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {:.4} | {:.4} | {} | {:.4} | {:.4} |\n",
            window.id,
            window.dataset,
            window.candidate_id,
            window.feature_count,
            window.validation.pnl,
            window.test.pnl,
            window.test.trades,
            window.test.roi,
            window.test.avg_entry
        ));
    }

    out.push_str("\n## Notes\n\n");
    for note in &report.notes {
        out.push_str(&format!("- {note}\n"));
    }

    out
}

pub fn build_event_ml_strategy_handoff(
    report: &WalkForwardReport,
    config: &EventMlStrategyHandoffConfig,
) -> EventMlStrategyHandoff {
    let runtime_score = config
        .runtime_score
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut blocked_gate_ids = report.readiness.missing_gate_ids.clone();
    if report.aggregate.total_test_pnl <= 0.0 {
        blocked_gate_ids.push("positive_total_test_pnl_missing".to_string());
    }
    if report.aggregate.positive_test_windows * 2 < report.aggregate.window_count {
        blocked_gate_ids.push("positive_test_window_majority_missing".to_string());
    }
    if runtime_score.is_none() {
        blocked_gate_ids.push("runtime_score_missing".to_string());
    }
    if !config.replay_parity_ready {
        blocked_gate_ids.push("replay_parity_missing".to_string());
    }

    let status = if blocked_gate_ids.is_empty() {
        EventMlStrategyHandoffStatus::Ready
    } else {
        EventMlStrategyHandoffStatus::Blocked
    };
    let strategy = match (&status, &runtime_score) {
        (EventMlStrategyHandoffStatus::Ready, Some(runtime_score)) => {
            Some(EventMlStrategyCandidate {
                strategy_profile: config.required_strategy_profile.clone(),
                runtime_score: runtime_score.clone(),
                selection_rule: report.selection_rule.clone(),
                window_count: report.aggregate.window_count,
                test_trades: report.aggregate.total_test_trades,
                test_pnl: report.aggregate.total_test_pnl,
                test_roi: report.aggregate.total_test_roi,
                weighted_avg_entry: report.aggregate.weighted_avg_entry,
                max_window_drawdown: report.aggregate.max_window_drawdown,
            })
        }
        _ => None,
    };

    EventMlStrategyHandoff {
        version: EVENT_ML_STRATEGY_HANDOFF_VERSION.to_string(),
        recommended_action: if status == EventMlStrategyHandoffStatus::Ready {
            "create_dry_run_handoff".to_string()
        } else {
            "do_not_promote".to_string()
        },
        required_strategy_profile: config.required_strategy_profile.clone(),
        runtime_score,
        replay_parity_ready: config.replay_parity_ready,
        blocked_gate_ids,
        promotion_gate: EventMlStrategyPromotionGate {
            walk_forward_status: report.readiness.status,
            source_report_path: config.source_report_path.clone(),
            aggregate: report.aggregate.clone(),
            missing_walk_forward_gate_ids: report.readiness.missing_gate_ids.clone(),
        },
        status,
        strategy,
    }
}

pub fn event_ml_strategy_handoff_markdown(handoff: &EventMlStrategyHandoff) -> String {
    let mut out = String::new();
    out.push_str("# Event ML Dry-Run Strategy Handoff\n\n");
    out.push_str(&format!("Version: `{}`\n\n", handoff.version));
    out.push_str(&format!("Status: `{}`\n\n", handoff.status.as_str()));
    out.push_str(&format!(
        "Recommended action: `{}`\n\n",
        handoff.recommended_action
    ));
    out.push_str(&format!(
        "- Required strategy profile: `{}`\n",
        handoff.required_strategy_profile
    ));
    out.push_str(&format!(
        "- Runtime score: `{}`\n",
        handoff.runtime_score.as_deref().unwrap_or("missing")
    ));
    out.push_str(&format!(
        "- Replay parity ready: `{}`\n",
        handoff.replay_parity_ready
    ));
    out.push_str(&format!(
        "- Walk-forward status: `{}`\n\n",
        handoff.promotion_gate.walk_forward_status.as_str()
    ));

    if handoff.status != EventMlStrategyHandoffStatus::Ready {
        out.push_str(
            "No dry-run handoff issue or config should be created from this artifact.\n\n",
        );
        out.push_str("## Blockers\n\n");
        for gate_id in &handoff.blocked_gate_ids {
            out.push_str(&format!("- `{gate_id}`\n"));
        }
        out.push('\n');
    }

    out.push_str("## Evidence\n\n");
    out.push_str(&format!(
        "- windows: `{}`\n",
        handoff.promotion_gate.aggregate.window_count
    ));
    out.push_str(&format!(
        "- test trades: `{}`\n",
        handoff.promotion_gate.aggregate.total_test_trades
    ));
    out.push_str(&format!(
        "- test PnL: `{:.4}`\n",
        handoff.promotion_gate.aggregate.total_test_pnl
    ));
    out.push_str(&format!(
        "- test ROI: `{:.4}`\n",
        handoff.promotion_gate.aggregate.total_test_roi
    ));
    out.push_str(&format!(
        "- weighted avg entry: `{:.4}`\n",
        handoff.promotion_gate.aggregate.weighted_avg_entry
    ));
    out.push_str(&format!(
        "- max window drawdown: `{:.4}`\n\n",
        handoff.promotion_gate.aggregate.max_window_drawdown
    ));

    if let Some(strategy) = &handoff.strategy {
        out.push_str("## Dry-Run Config Contract\n\n");
        out.push_str("```toml\n");
        out.push_str("[event_ml_strategy_handoff]\n");
        out.push_str(&format!(
            "strategy_profile = \"{}\"\n",
            strategy.strategy_profile
        ));
        out.push_str(&format!("runtime_score = \"{}\"\n", strategy.runtime_score));
        out.push_str("promotion_status = \"ready_for_dry_run_handoff\"\n");
        out.push_str("```\n\n");
        out.push_str("## Monitoring Requirements\n\n");
        out.push_str("- Confirm runtime/replay scorer parity before deployment.\n");
        out.push_str("- Confirm live dry-run quote availability matches research assumptions.\n");
        out.push_str("- Keep live trading disabled until a separate operator approval gate.\n");
    }

    out
}

struct LoadedWindow {
    selection_rule: String,
    window: WalkForwardWindow,
}

fn load_window(id: usize, run_dir: &Path) -> Result<LoadedWindow> {
    let workflow_path = run_dir.join("workflow_report.json");
    let workflow: WorkflowReportArtifact = read_json(&workflow_path)?;

    let search_path = run_dir
        .join("hyperparameter")
        .join("hyperparameter_search.json");
    let search: HyperparameterSearchArtifact = read_json(&search_path)?;
    let candidate = search
        .best_candidate
        .context("hyperparameter_search.json missing best_candidate")?;

    let baseline_path = resolve_candidate_path(run_dir, &candidate.output_json);
    let baseline: BaselineArtifact = read_json(&baseline_path)?;
    let validation = metric_for_split(&baseline, "val")?;
    let test = metric_for_split(&baseline, "test")?;

    Ok(LoadedWindow {
        selection_rule: search.selection_rule,
        window: WalkForwardWindow {
            id,
            run_dir: run_dir.display().to_string(),
            dataset: workflow.dataset,
            entry_secs: workflow.entry_secs,
            tolerance_secs: workflow.tolerance_secs,
            candidate_id: candidate.id,
            feature_count: workflow.governed_features.len(),
            baseline_metrics_path: baseline_path.display().to_string(),
            validation,
            test,
        },
    })
}

fn read_json<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    serde_json::from_reader(file).with_context(|| format!("parse {}", path.display()))
}

fn resolve_candidate_path(run_dir: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        run_dir.join(path)
    }
}

fn metric_for_split(artifact: &BaselineArtifact, split: &str) -> Result<WalkForwardMetric> {
    let metric = artifact
        .metrics
        .iter()
        .find(|metric| metric.split == split)
        .with_context(|| format!("baseline artifact missing {split} metric"))?;
    let win_rate = if metric.trades > 0 {
        metric.wins as f64 / metric.trades as f64
    } else {
        0.0
    };
    Ok(WalkForwardMetric {
        trades: metric.trades,
        wins: metric.wins,
        win_rate,
        pnl: metric.pnl,
        cost: metric.cost,
        roi: metric.roi,
        avg_entry: metric.avg_entry,
        logloss: metric.logloss,
        auc: metric.auc,
    })
}

fn aggregate_windows(windows: &[WalkForwardWindow]) -> WalkForwardAggregate {
    let window_count = windows.len();
    let total_test_trades = windows.iter().map(|window| window.test.trades).sum();
    let total_test_cost = windows.iter().map(|window| window.test.cost).sum();
    let total_test_pnl = windows.iter().map(|window| window.test.pnl).sum();
    let total_test_roi = if total_test_cost > 0.0 {
        total_test_pnl / total_test_cost
    } else {
        0.0
    };
    let weighted_avg_entry = weighted_avg_entry(windows, total_test_trades);
    let mean_test_logloss = mean_by_window(windows, |window| window.test.logloss);
    let mean_test_pnl = mean_by_window(windows, |window| window.test.pnl);
    let positive_test_windows = windows
        .iter()
        .filter(|window| window.test.pnl > 0.0)
        .count();
    let validation_test_direction_agreement = windows
        .iter()
        .filter(|window| same_direction(window.validation.pnl, window.test.pnl))
        .count();
    let max_window_drawdown = max_drawdown(windows);

    WalkForwardAggregate {
        window_count,
        total_test_trades,
        total_test_cost,
        total_test_pnl,
        total_test_roi,
        weighted_avg_entry,
        mean_test_logloss,
        mean_test_pnl,
        positive_test_windows,
        validation_test_direction_agreement,
        max_window_drawdown,
    }
}

fn weighted_avg_entry(windows: &[WalkForwardWindow], total_trades: usize) -> f64 {
    if total_trades == 0 {
        return 0.0;
    }
    windows
        .iter()
        .map(|window| window.test.avg_entry * window.test.trades as f64)
        .sum::<f64>()
        / total_trades as f64
}

fn mean_by_window<F>(windows: &[WalkForwardWindow], metric: F) -> f64
where
    F: Fn(&WalkForwardWindow) -> f64,
{
    if windows.is_empty() {
        return 0.0;
    }
    windows.iter().map(metric).sum::<f64>() / windows.len() as f64
}

fn same_direction(left: f64, right: f64) -> bool {
    (left >= 0.0 && right >= 0.0) || (left <= 0.0 && right <= 0.0)
}

fn max_drawdown(windows: &[WalkForwardWindow]) -> f64 {
    let mut equity = 0.0;
    let mut peak = 0.0;
    let mut drawdown: f64 = 0.0;
    for window in windows {
        equity += window.test.pnl;
        peak = f64::max(peak, equity);
        drawdown = drawdown.max(peak - equity);
    }
    drawdown
}

fn readiness_gates(
    config: &WalkForwardConfig,
    windows: &[WalkForwardWindow],
    aggregate: &WalkForwardAggregate,
) -> Vec<WalkForwardGate> {
    vec![
        WalkForwardGate {
            id: "min_walk_forward_windows".to_string(),
            passed: windows.len() >= config.min_windows,
            evidence: format!(
                "windows={} min_required={}",
                windows.len(),
                config.min_windows
            ),
            stop_if_missing: "collect more event-root windows before DL/RL".to_string(),
        },
        WalkForwardGate {
            id: "unique_dataset_windows".to_string(),
            passed: unique_dataset_count(windows) >= config.min_windows,
            evidence: format!(
                "unique_datasets={} min_required={}",
                unique_dataset_count(windows),
                config.min_windows
            ),
            stop_if_missing:
                "run distinct rolling event-root datasets instead of reusing one split".to_string(),
        },
        WalkForwardGate {
            id: "test_trades_present".to_string(),
            passed: windows
                .iter()
                .all(|window| window.test.trades >= config.min_test_trades_per_window),
            evidence: format!(
                "min_test_trades_per_window={} total_test_trades={}",
                config.min_test_trades_per_window, aggregate.total_test_trades
            ),
            stop_if_missing: "fix tradable coverage before model escalation".to_string(),
        },
        WalkForwardGate {
            id: "executable_entry_accounting".to_string(),
            passed: windows.iter().all(|window| {
                window.test.cost > 0.0 && window.test.avg_entry > 0.0 && window.test.avg_entry < 1.0
            }),
            evidence: format!(
                "total_cost={:.4} weighted_avg_entry={:.4}",
                aggregate.total_test_cost, aggregate.weighted_avg_entry
            ),
            stop_if_missing: "do not claim executable strategy quality".to_string(),
        },
        WalkForwardGate {
            id: "window_drawdown_reported".to_string(),
            passed: aggregate.max_window_drawdown.is_finite(),
            evidence: format!("max_window_drawdown={:.4}", aggregate.max_window_drawdown),
            stop_if_missing: "add drawdown evidence before dry-run handoff".to_string(),
        },
        WalkForwardGate {
            id: "validation_test_direction_reported".to_string(),
            passed: true,
            evidence: format!(
                "agreement_windows={} of {}",
                aggregate.validation_test_direction_agreement, aggregate.window_count
            ),
            stop_if_missing: "report validation/test drift before model-family escalation"
                .to_string(),
        },
    ]
}

fn ensure_unique_run_dirs(run_dirs: &[PathBuf]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for run_dir in run_dirs {
        let key = run_dir.display().to_string();
        if !seen.insert(key.clone()) {
            bail!("duplicate walk-forward run dir: {key}");
        }
    }
    Ok(())
}

fn unique_dataset_count(windows: &[WalkForwardWindow]) -> usize {
    windows
        .iter()
        .map(|window| window.dataset.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(
        id: usize,
        val_pnl: f64,
        test_pnl: f64,
        trades: usize,
        avg_entry: f64,
    ) -> WalkForwardWindow {
        WalkForwardWindow {
            id,
            run_dir: format!("/tmp/run-{id}"),
            dataset: format!("dataset-{id}"),
            entry_secs: 60,
            tolerance_secs: 30,
            candidate_id: 1,
            feature_count: 8,
            baseline_metrics_path: format!("/tmp/run-{id}/baseline_metrics.json"),
            validation: WalkForwardMetric {
                trades,
                wins: trades / 2,
                win_rate: 0.5,
                pnl: val_pnl,
                cost: trades as f64 * avg_entry,
                roi: 0.0,
                avg_entry,
                logloss: 0.5,
                auc: Some(0.6),
            },
            test: WalkForwardMetric {
                trades,
                wins: trades / 2,
                win_rate: 0.5,
                pnl: test_pnl,
                cost: trades as f64 * avg_entry,
                roi: if trades > 0 {
                    test_pnl / (trades as f64 * avg_entry)
                } else {
                    0.0
                },
                avg_entry,
                logloss: 0.6,
                auc: Some(0.55),
            },
        }
    }

    #[test]
    fn aggregate_reports_window_level_drawdown() {
        let windows = vec![
            window(1, 1.0, 2.0, 10, 0.4),
            window(2, 1.0, -3.0, 10, 0.5),
            window(3, 1.0, 1.0, 10, 0.5),
        ];

        let aggregate = aggregate_windows(&windows);

        assert_eq!(aggregate.window_count, 3);
        assert_eq!(aggregate.total_test_trades, 30);
        assert!((aggregate.total_test_pnl - 0.0).abs() < 1e-9);
        assert!((aggregate.max_window_drawdown - 3.0).abs() < 1e-9);
    }

    #[test]
    fn single_window_is_blocked_by_min_window_gate() {
        let config = WalkForwardConfig {
            run_dirs: vec![PathBuf::from("/tmp/run-1")],
            min_windows: 3,
            min_test_trades_per_window: 1,
        };
        let windows = vec![window(1, 1.0, 1.0, 5, 0.4)];
        let aggregate = aggregate_windows(&windows);
        let gates = readiness_gates(&config, &windows, &aggregate);

        assert!(gates
            .iter()
            .any(|gate| gate.id == "min_walk_forward_windows" && !gate.passed));
    }

    #[test]
    fn duplicate_dataset_windows_do_not_satisfy_rolling_gate() {
        let config = WalkForwardConfig {
            run_dirs: vec![
                PathBuf::from("/tmp/run-1"),
                PathBuf::from("/tmp/run-2"),
                PathBuf::from("/tmp/run-3"),
            ],
            min_windows: 3,
            min_test_trades_per_window: 1,
        };
        let mut windows = vec![
            window(1, 1.0, 1.0, 5, 0.4),
            window(2, 1.0, 1.0, 5, 0.4),
            window(3, 1.0, 1.0, 5, 0.4),
        ];
        for window in &mut windows {
            window.dataset = "same-dataset".to_string();
        }
        let aggregate = aggregate_windows(&windows);
        let gates = readiness_gates(&config, &windows, &aggregate);

        assert!(gates
            .iter()
            .any(|gate| gate.id == "unique_dataset_windows" && !gate.passed));
    }

    #[test]
    fn duplicate_run_dirs_are_rejected() {
        let error =
            ensure_unique_run_dirs(&[PathBuf::from("/tmp/run-1"), PathBuf::from("/tmp/run-1")])
                .unwrap_err()
                .to_string();

        assert!(error.contains("duplicate walk-forward run dir"));
    }

    #[test]
    fn event_ml_handoff_is_blocked_without_runtime_and_replay_parity() {
        let windows = vec![
            window(1, 1.0, 2.0, 10, 0.4),
            window(2, 1.0, 2.0, 10, 0.4),
            window(3, 1.0, 2.0, 10, 0.4),
        ];
        let report = WalkForwardReport {
            version: WALK_FORWARD_REPORT_VERSION.to_string(),
            selection_rule: "test rule".to_string(),
            readiness: WalkForwardReadiness {
                ready_for_dl_rl: true,
                status: WalkForwardGateStatus::Ready,
                missing_gate_ids: vec![],
            },
            gates: vec![],
            aggregate: aggregate_windows(&windows),
            windows,
            notes: vec![],
        };

        let handoff =
            build_event_ml_strategy_handoff(&report, &EventMlStrategyHandoffConfig::default());

        assert_eq!(handoff.status, EventMlStrategyHandoffStatus::Blocked);
        assert_eq!(handoff.recommended_action, "do_not_promote");
        assert!(handoff.strategy.is_none());
        assert!(handoff
            .blocked_gate_ids
            .contains(&"runtime_score_missing".to_string()));
        assert!(handoff
            .blocked_gate_ids
            .contains(&"replay_parity_missing".to_string()));
    }

    #[test]
    fn event_ml_handoff_treats_blank_runtime_score_as_missing() {
        let windows = vec![
            window(1, 1.0, 2.0, 10, 0.4),
            window(2, 1.0, 1.0, 10, 0.4),
            window(3, 1.0, 1.0, 10, 0.4),
        ];
        let report = WalkForwardReport {
            version: WALK_FORWARD_REPORT_VERSION.to_string(),
            selection_rule: "validation pnl".to_string(),
            readiness: WalkForwardReadiness {
                ready_for_dl_rl: true,
                status: WalkForwardGateStatus::Ready,
                missing_gate_ids: vec![],
            },
            gates: vec![],
            aggregate: aggregate_windows(&windows),
            windows,
            notes: vec![],
        };
        let config = EventMlStrategyHandoffConfig {
            runtime_score: Some("   ".to_string()),
            replay_parity_ready: true,
            ..EventMlStrategyHandoffConfig::default()
        };

        let handoff = build_event_ml_strategy_handoff(&report, &config);

        assert_eq!(handoff.status, EventMlStrategyHandoffStatus::Blocked);
        assert!(handoff.runtime_score.is_none());
        assert!(handoff
            .blocked_gate_ids
            .contains(&"runtime_score_missing".to_string()));
    }

    #[test]
    fn event_ml_handoff_is_ready_only_after_all_strategy_gates() {
        let windows = vec![
            window(1, 1.0, 2.0, 10, 0.4),
            window(2, 1.0, 1.0, 10, 0.4),
            window(3, 1.0, 1.0, 10, 0.4),
        ];
        let report = WalkForwardReport {
            version: WALK_FORWARD_REPORT_VERSION.to_string(),
            selection_rule: "validation pnl".to_string(),
            readiness: WalkForwardReadiness {
                ready_for_dl_rl: true,
                status: WalkForwardGateStatus::Ready,
                missing_gate_ids: vec![],
            },
            gates: vec![],
            aggregate: aggregate_windows(&windows),
            windows,
            notes: vec![],
        };
        let config = EventMlStrategyHandoffConfig {
            runtime_score: Some("event_ml_model:baseline_v1".to_string()),
            replay_parity_ready: true,
            source_report_path: Some("walk_forward_report.json".to_string()),
            ..EventMlStrategyHandoffConfig::default()
        };

        let handoff = build_event_ml_strategy_handoff(&report, &config);

        assert_eq!(handoff.status, EventMlStrategyHandoffStatus::Ready);
        assert_eq!(handoff.recommended_action, "create_dry_run_handoff");
        assert!(handoff.blocked_gate_ids.is_empty());
        let strategy = handoff.strategy.expect("ready handoff has strategy");
        assert_eq!(strategy.runtime_score, "event_ml_model:baseline_v1");
        assert_eq!(strategy.window_count, 3);
        assert_eq!(strategy.test_trades, 30);
    }
}
