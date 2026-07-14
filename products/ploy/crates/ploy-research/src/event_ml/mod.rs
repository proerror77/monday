pub mod walk_forward;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub use walk_forward::{
    build_event_ml_strategy_handoff, build_walk_forward_report, event_ml_strategy_handoff_markdown,
    walk_forward_report_markdown, EventMlStrategyCandidate, EventMlStrategyHandoff,
    EventMlStrategyHandoffConfig, EventMlStrategyHandoffStatus, EventMlStrategyPromotionGate,
    WalkForwardAggregate, WalkForwardConfig, WalkForwardGate, WalkForwardGateStatus,
    WalkForwardMetric, WalkForwardReadiness, WalkForwardReport, WalkForwardWindow,
    EVENT_ML_STRATEGY_HANDOFF_VERSION, WALK_FORWARD_REPORT_VERSION,
};

pub const EVENT_ML_ARCHITECTURE_VERSION: &str = "event-ml-architecture.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventMlArchitecture {
    pub version: String,
    pub purpose: String,
    pub phases: Vec<WorkflowPhase>,
    pub learning_lanes: Vec<LearningLane>,
    pub artifacts: Vec<ArchitectureArtifact>,
    pub readiness_gates: Vec<ReadinessGate>,
    pub stop_rules: Vec<String>,
}

impl EventMlArchitecture {
    pub fn phase(&self, id: PhaseId) -> Option<&WorkflowPhase> {
        self.phases.iter().find(|phase| phase.id == id)
    }

    pub fn learning_lane(&self, id: LearningLaneId) -> Option<&LearningLane> {
        self.learning_lanes.iter().find(|lane| lane.id == id)
    }

    pub fn gates_for_lane(&self, id: LearningLaneId) -> Vec<&ReadinessGate> {
        self.readiness_gates
            .iter()
            .filter(|gate| gate.required_for.contains(&id))
            .collect()
    }

    pub fn evaluate_lane_readiness<I, S>(
        &self,
        id: LearningLaneId,
        satisfied_gate_ids: I,
    ) -> LaneReadiness
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let satisfied: BTreeSet<String> = satisfied_gate_ids
            .into_iter()
            .map(|gate_id| gate_id.as_ref().to_string())
            .collect();
        let required: Vec<String> = self
            .gates_for_lane(id)
            .into_iter()
            .map(|gate| gate.id.clone())
            .collect();
        let missing: Vec<String> = required
            .iter()
            .filter(|gate_id| !satisfied.contains(*gate_id))
            .cloned()
            .collect();

        LaneReadiness {
            lane: id,
            ready: missing.is_empty(),
            required_gate_ids: required,
            satisfied_gate_ids: satisfied.into_iter().collect(),
            missing_gate_ids: missing,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowPhase {
    pub id: PhaseId,
    pub order: u8,
    pub title: String,
    pub goal: String,
    pub required_inputs: Vec<String>,
    pub required_outputs: Vec<String>,
    pub stop_gate: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PhaseId {
    DatasetContract,
    CoverageDiagnostics,
    AutomlFactorAttribution,
    FeatureGovernance,
    FixedBaseline,
    ModelFamilySelection,
    HyperparameterSearch,
    WalkForwardBacktest,
    DlGate,
    RlGate,
    DryRunHandoff,
}

impl PhaseId {
    pub const fn as_str(self) -> &'static str {
        match self {
            PhaseId::DatasetContract => "dataset_contract",
            PhaseId::CoverageDiagnostics => "coverage_diagnostics",
            PhaseId::AutomlFactorAttribution => "automl_factor_attribution",
            PhaseId::FeatureGovernance => "feature_governance",
            PhaseId::FixedBaseline => "fixed_baseline",
            PhaseId::ModelFamilySelection => "model_family_selection",
            PhaseId::HyperparameterSearch => "hyperparameter_search",
            PhaseId::WalkForwardBacktest => "walk_forward_backtest",
            PhaseId::DlGate => "dl_gate",
            PhaseId::RlGate => "rl_gate",
            PhaseId::DryRunHandoff => "dry_run_handoff",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LearningLane {
    pub id: LearningLaneId,
    pub title: String,
    pub status: LaneStatus,
    pub starts_after: Vec<PhaseId>,
    pub purpose: String,
    pub allowed_work: Vec<String>,
    pub blocked_until: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LearningLaneId {
    SupervisedTabular,
    DeepLearning,
    ReinforcementLearning,
    DryRunStrategy,
}

impl LearningLaneId {
    pub const fn as_str(self) -> &'static str {
        match self {
            LearningLaneId::SupervisedTabular => "supervised_tabular",
            LearningLaneId::DeepLearning => "deep_learning",
            LearningLaneId::ReinforcementLearning => "reinforcement_learning",
            LearningLaneId::DryRunStrategy => "dry_run_strategy",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaneStatus {
    ActiveFoundation,
    Gated,
}

impl LaneStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            LaneStatus::ActiveFoundation => "active_foundation",
            LaneStatus::Gated => "gated",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArchitectureArtifact {
    pub id: String,
    pub path_pattern: String,
    pub produced_by: PhaseId,
    pub consumed_by: Vec<PhaseId>,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadinessGate {
    pub id: String,
    pub title: String,
    pub required_for: Vec<LearningLaneId>,
    pub evidence: Vec<String>,
    pub stop_if_missing: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaneReadiness {
    pub lane: LearningLaneId,
    pub ready: bool,
    pub required_gate_ids: Vec<String>,
    pub satisfied_gate_ids: Vec<String>,
    pub missing_gate_ids: Vec<String>,
}

pub fn canonical_event_ml_architecture() -> EventMlArchitecture {
    EventMlArchitecture {
        version: EVENT_ML_ARCHITECTURE_VERSION.to_string(),
        purpose: "Shared contract for event-held-out PM5D ML, DL, RL, and dry-run handoff work."
            .to_string(),
        phases: canonical_phases(),
        learning_lanes: canonical_learning_lanes(),
        artifacts: canonical_artifacts(),
        readiness_gates: canonical_readiness_gates(),
        stop_rules: vec![
            "Never train when train, validation, and test event IDs overlap.".to_string(),
            "Never define factor direction from validation or test splits.".to_string(),
            "Never select hyperparameters or model family on test metrics.".to_string(),
            "Never advance DL or RL before executable-price accounting is stable.".to_string(),
            "Never claim strategy quality from win rate without entry price, payout, and drawdown evidence."
                .to_string(),
        ],
    }
}

pub fn event_ml_architecture_markdown(architecture: &EventMlArchitecture) -> String {
    let mut out = String::new();
    out.push_str("# Event ML Foundation Architecture\n\n");
    out.push_str(&format!("Version: `{}`\n\n", architecture.version));
    out.push_str(&format!("{}\n\n", architecture.purpose));

    out.push_str("## Canonical Phases\n\n");
    for phase in &architecture.phases {
        out.push_str(&format!(
            "{}. **{}** (`{}`)\n\n",
            phase.order,
            phase.title,
            phase.id.as_str()
        ));
        out.push_str(&format!("- Goal: {}\n", phase.goal));
        out.push_str(&format!(
            "- Outputs: {}\n",
            phase.required_outputs.join(", ")
        ));
        out.push_str(&format!("- Stop gate: {}\n\n", phase.stop_gate));
    }

    out.push_str("## Learning Lanes\n\n");
    for lane in &architecture.learning_lanes {
        out.push_str(&format!("### {}\n\n", lane.title));
        out.push_str(&format!("- Id: `{}`\n", lane.id.as_str()));
        out.push_str(&format!("- Status: `{}`\n", lane.status.as_str()));
        out.push_str(&format!("- Purpose: {}\n", lane.purpose));
        out.push_str(&format!(
            "- Allowed work: {}\n",
            lane.allowed_work.join("; ")
        ));
        out.push_str(&format!(
            "- Blocked until: {}\n\n",
            lane.blocked_until.join("; ")
        ));
    }

    out.push_str("## Artifacts\n\n");
    for artifact in &architecture.artifacts {
        out.push_str(&format!(
            "- `{}` -> `{}`: {}\n",
            artifact.id, artifact.path_pattern, artifact.description
        ));
    }

    out.push_str("\n## Readiness Gates\n\n");
    for gate in &architecture.readiness_gates {
        out.push_str(&format!("- `{}`: {}\n", gate.id, gate.title));
        out.push_str(&format!("  Evidence: {}\n", gate.evidence.join("; ")));
        out.push_str(&format!("  Stop if missing: {}\n", gate.stop_if_missing));
    }

    out.push_str("\n## Stop Rules\n\n");
    for rule in &architecture.stop_rules {
        out.push_str(&format!("- {}\n", rule));
    }

    out
}

fn canonical_phases() -> Vec<WorkflowPhase> {
    vec![
        phase(
            PhaseId::DatasetContract,
            0,
            "Dataset contract",
            "Prove the event-root dataset is trustworthy ML input.",
            &["event_manifest.json", "observation split Parquet files"],
            &["validated manifest", "disjoint event split proof"],
            "Stop if split event IDs overlap or labels/prices are invalid.",
        ),
        phase(
            PhaseId::CoverageDiagnostics,
            1,
            "Coverage diagnostics",
            "Measure selected-row and feature coverage before training.",
            &["event-root dataset"],
            &["coverage report", "feature missingness diagnostics"],
            "Stop if row-complete tradable coverage is too low for the claimed model quality.",
        ),
        phase(
            PhaseId::AutomlFactorAttribution,
            2,
            "AutoML factor attribution",
            "Rank candidate factors and register train-derived direction metadata.",
            &["coverage-passing dataset", "candidate features"],
            &[
                "factor_attributions.json",
                "event_ml_factor_registry.json",
                "event_ml_factor_registry.md",
            ],
            "Stop if validation lift contradicts train direction or coverage is too thin.",
        ),
        phase(
            PhaseId::FeatureGovernance,
            3,
            "Feature governance",
            "Freeze the feature schema before comparing models.",
            &["factor attribution report"],
            &["feature_whitelist.txt", "feature_whitelist.md"],
            "Stop if each trial is allowed to silently choose a different feature set.",
        ),
        phase(
            PhaseId::FixedBaseline,
            4,
            "Fixed supervised baseline",
            "Establish a train-only normalized logistic baseline with executable entry accounting.",
            &["feature whitelist", "event-held-out splits"],
            &["baseline_metrics.json", "embedded model contract"],
            "Stop if the fixed baseline cannot report split metrics and simple PnL.",
        ),
        phase(
            PhaseId::ModelFamilySelection,
            5,
            "Model family selection",
            "Choose model family after the governed baseline is stable.",
            &["baseline metrics", "feature whitelist"],
            &["model_family_decision.json"],
            "Stop if complexity wins only in validation and fails stability checks.",
        ),
        phase(
            PhaseId::HyperparameterSearch,
            6,
            "Hyperparameter search",
            "Tune a selected model family with validation-only selection.",
            &["model family decision", "feature whitelist"],
            &["hyperparameter_search.json", "candidate baseline_metrics.json files"],
            "Stop if any candidate is selected using test metrics.",
        ),
        phase(
            PhaseId::WalkForwardBacktest,
            7,
            "Walk-forward and executable-price backtest",
            "Prove the result is not one lucky split and report bankroll risk.",
            &["selected model config", "multi-day event-root data"],
            &[
                "walk_forward_report.json",
                "event_ml_strategy_handoff.json",
                "backtest_report.json",
            ],
            "Stop if validation improvement does not survive rolling windows or executable accounting.",
        ),
        phase(
            PhaseId::DlGate,
            8,
            "DL readiness gate",
            "Permit deep learning only when sequence/state questions are justified by enough data.",
            &["walk-forward evidence", "state representation contract"],
            &["dl_readiness.json"],
            "Stop if the tabular workflow is unstable or the dataset is too small/noisy.",
        ),
        phase(
            PhaseId::RlGate,
            9,
            "RL readiness gate",
            "Permit reinforcement learning only when the replay environment matches execution semantics.",
            &["executable backtest", "state/action/reward contract"],
            &["rl_readiness.json"],
            "Stop if reward, quote availability, latency, or bankroll accounting is missing.",
        ),
        phase(
            PhaseId::DryRunHandoff,
            10,
            "Dry-run strategy handoff",
            "Package the model and evidence for dry-run without changing live behavior.",
            &["research artifacts", "backtest evidence"],
            &[
                "event_ml_strategy_handoff.json",
                "event_ml_strategy_handoff.md",
                "model version metadata",
            ],
            "Stop if model version, feature schema, entry window, and monitoring requirements are absent.",
        ),
    ]
}

fn canonical_learning_lanes() -> Vec<LearningLane> {
    vec![
        LearningLane {
            id: LearningLaneId::SupervisedTabular,
            title: "Supervised tabular ML".to_string(),
            status: LaneStatus::ActiveFoundation,
            starts_after: vec![PhaseId::FeatureGovernance],
            purpose: "Build the first reliable event-held-out learning baseline.".to_string(),
            allowed_work: vec![
                "fixed logistic baseline".to_string(),
                "regularized linear/logistic models".to_string(),
                "small tree or shallow ensemble comparisons".to_string(),
                "bounded hyperparameter search after model-family selection".to_string(),
            ],
            blocked_until: vec![
                "coverage diagnostics pass".to_string(),
                "feature whitelist is frozen".to_string(),
                "train-only normalization is enforced".to_string(),
            ],
        },
        LearningLane {
            id: LearningLaneId::DeepLearning,
            title: "Deep learning".to_string(),
            status: LaneStatus::Gated,
            starts_after: vec![PhaseId::WalkForwardBacktest, PhaseId::DlGate],
            purpose: "Test whether sequence state or nonlinear interactions beat the governed tabular baseline."
                .to_string(),
            allowed_work: vec![
                "small MLP only after tabular baselines are stable".to_string(),
                "sequence model only after multi-row event state is defined".to_string(),
                "calibration experiments with validation-only selection".to_string(),
            ],
            blocked_until: vec![
                "enough multi-day events exist".to_string(),
                "walk-forward tabular baseline is stable".to_string(),
                "no-future-row sequence/state contract is written".to_string(),
            ],
        },
        LearningLane {
            id: LearningLaneId::ReinforcementLearning,
            title: "Reinforcement learning".to_string(),
            status: LaneStatus::Gated,
            starts_after: vec![PhaseId::WalkForwardBacktest, PhaseId::RlGate],
            purpose: "Evaluate dynamic entry, exit, or sizing only after execution semantics are faithful."
                .to_string(),
            allowed_work: vec![
                "offline replay environment parity tests".to_string(),
                "explicit no-trade/buy-up/buy-down action policy".to_string(),
                "reward and bankroll accounting checks".to_string(),
            ],
            blocked_until: vec![
                "decision-time-only state is proven".to_string(),
                "reward matches binary payout and entry price".to_string(),
                "quote availability, latency, sizing, and bankroll accounting are modeled".to_string(),
            ],
        },
        LearningLane {
            id: LearningLaneId::DryRunStrategy,
            title: "Dry-run strategy handoff".to_string(),
            status: LaneStatus::Gated,
            starts_after: vec![PhaseId::DryRunHandoff],
            purpose: "Convert validated research into observable dry-run metadata without live promotion."
                .to_string(),
            allowed_work: vec![
                "model-versioned dry-run signal packet".to_string(),
                "feature schema and entry-window monitoring".to_string(),
                "quote/fill/latency observability checks".to_string(),
            ],
            blocked_until: vec![
                "handoff packet is complete".to_string(),
                "walk-forward and executable-price reports are attached".to_string(),
            ],
        },
    ]
}

fn canonical_artifacts() -> Vec<ArchitectureArtifact> {
    vec![
        artifact(
            "dataset_manifest",
            "event_manifest.json",
            PhaseId::DatasetContract,
            &[PhaseId::CoverageDiagnostics, PhaseId::FixedBaseline],
            "Dataset source, split, label, and export contract.",
        ),
        artifact(
            "observations",
            "observations_{train,val,test}.parquet",
            PhaseId::DatasetContract,
            &[PhaseId::CoverageDiagnostics, PhaseId::AutomlFactorAttribution, PhaseId::FixedBaseline],
            "Event-held-out observation splits.",
        ),
        artifact(
            "factor_attributions",
            "factor_attributions.json",
            PhaseId::AutomlFactorAttribution,
            &[PhaseId::FeatureGovernance, PhaseId::ModelFamilySelection],
            "AutoML-style split diagnostics and factor registry metadata.",
        ),
        artifact(
            "feature_whitelist",
            "feature_whitelist.{txt,md}",
            PhaseId::FeatureGovernance,
            &[PhaseId::FixedBaseline, PhaseId::ModelFamilySelection, PhaseId::HyperparameterSearch],
            "Frozen governed feature schema.",
        ),
        artifact(
            "baseline_metrics",
            "baseline_metrics.json",
            PhaseId::FixedBaseline,
            &[PhaseId::ModelFamilySelection, PhaseId::HyperparameterSearch],
            "Fixed supervised baseline split metrics and one-event-one-trade PnL.",
        ),
        artifact(
            "hyperparameter_search",
            "hyperparameter_search.{json,md}",
            PhaseId::HyperparameterSearch,
            &[PhaseId::WalkForwardBacktest],
            "Validation-selected candidate record with test metrics kept as reporting evidence only.",
        ),
        artifact(
            "walk_forward_report",
            "walk_forward_report.{json,md}",
            PhaseId::WalkForwardBacktest,
            &[PhaseId::DlGate, PhaseId::RlGate, PhaseId::DryRunHandoff],
            "Rolling-window OOS stability and executable-price accounting evidence.",
        ),
        artifact(
            "dl_readiness",
            "dl_readiness.json",
            PhaseId::DlGate,
            &[PhaseId::DryRunHandoff],
            "Explicit decision record for whether DL is justified.",
        ),
        artifact(
            "rl_readiness",
            "rl_readiness.json",
            PhaseId::RlGate,
            &[PhaseId::DryRunHandoff],
            "Explicit decision record for whether RL environment work is justified.",
        ),
        artifact(
            "strategy_handoff",
            "event_ml_strategy_handoff.{json,md}",
            PhaseId::DryRunHandoff,
            &[],
            "Research-to-dry-run packet with model version, schema, entry window, and monitoring requirements.",
        ),
    ]
}

fn canonical_readiness_gates() -> Vec<ReadinessGate> {
    vec![
        gate(
            "event_split_disjoint",
            "Train, validation, and test event IDs are disjoint.",
            &[
                LearningLaneId::SupervisedTabular,
                LearningLaneId::DeepLearning,
                LearningLaneId::ReinforcementLearning,
                LearningLaneId::DryRunStrategy,
            ],
            &["manifest split proof", "baseline split overlap check"],
            "stop all learning work",
        ),
        gate(
            "train_only_normalization",
            "Feature normalization uses train data only.",
            &[
                LearningLaneId::SupervisedTabular,
                LearningLaneId::DeepLearning,
                LearningLaneId::DryRunStrategy,
            ],
            &["normalization metadata", "baseline implementation evidence"],
            "stop model comparison",
        ),
        gate(
            "tradable_entry_coverage",
            "Selected rows have enough valid labels, prices, and finite features.",
            &[
                LearningLaneId::SupervisedTabular,
                LearningLaneId::DeepLearning,
                LearningLaneId::DryRunStrategy,
            ],
            &["coverage report", "selected row counts by split"],
            "fix data/export or narrow features before training",
        ),
        gate(
            "governed_feature_schema",
            "Feature whitelist is fixed before model comparison.",
            &[
                LearningLaneId::SupervisedTabular,
                LearningLaneId::DeepLearning,
                LearningLaneId::DryRunStrategy,
            ],
            &["feature_whitelist.txt", "rejected-feature notes"],
            "stop model-family or hyperparameter selection",
        ),
        gate(
            "fixed_baseline_metrics",
            "Fixed baseline reports split metrics and executable entry PnL.",
            &[
                LearningLaneId::SupervisedTabular,
                LearningLaneId::DeepLearning,
                LearningLaneId::DryRunStrategy,
            ],
            &[
                "baseline_metrics.json",
                "accuracy/logloss/Brier/AUC/PnL/ROI/avg_entry",
            ],
            "stop DL, RL, and strategy handoff",
        ),
        gate(
            "validation_only_selection",
            "Model family and parameters are selected without test leakage.",
            &[
                LearningLaneId::SupervisedTabular,
                LearningLaneId::DeepLearning,
                LearningLaneId::DryRunStrategy,
            ],
            &["model_family_decision.json", "hyperparameter_search.json"],
            "discard candidate selection",
        ),
        gate(
            "walk_forward_stability",
            "Performance survives rolling windows or multi-day OOS splits.",
            &[
                LearningLaneId::DeepLearning,
                LearningLaneId::ReinforcementLearning,
                LearningLaneId::DryRunStrategy,
            ],
            &["walk_forward_report.json", "per-window trade/event counts"],
            "continue supervised data work instead",
        ),
        gate(
            "executable_price_accounting",
            "PnL uses executable entry quotes with payout, costs, and drawdown framing.",
            &[
                LearningLaneId::DeepLearning,
                LearningLaneId::ReinforcementLearning,
                LearningLaneId::DryRunStrategy,
            ],
            &[
                "backtest_report.json",
                "avg_entry",
                "payout",
                "drawdown",
                "bankroll risk",
            ],
            "do not claim strategy quality",
        ),
        gate(
            "dl_min_history",
            "DL has enough multi-day events to test sequence or nonlinear questions.",
            &[LearningLaneId::DeepLearning],
            &["multi-day event count", "OOS test event count"],
            "do not train DL because the dataset is too small/noisy",
        ),
        gate(
            "dl_state_contract",
            "DL state representation excludes future rows and settlement leakage.",
            &[LearningLaneId::DeepLearning],
            &["state schema", "entry-window contract", "leakage review"],
            "do not start sequence or neural training",
        ),
        gate(
            "rl_decision_time_state",
            "RL state includes only information available at decision time.",
            &[LearningLaneId::ReinforcementLearning],
            &[
                "state schema",
                "quote timestamp proof",
                "future-row exclusion tests",
            ],
            "do not create an RL training environment",
        ),
        gate(
            "rl_action_space",
            "RL action space is explicit and tied to tradable decisions.",
            &[LearningLaneId::ReinforcementLearning],
            &[
                "no_trade/buy_up/buy_down action contract",
                "optional exit contract",
            ],
            "do not train an agent against ambiguous actions",
        ),
        gate(
            "rl_binary_reward",
            "RL reward matches binary payout, entry price, fees, and position outcome.",
            &[LearningLaneId::ReinforcementLearning],
            &["reward formula", "parity with supervised backtest PnL"],
            "do not optimize a reward that cannot settle like the market",
        ),
        gate(
            "rl_latency_bankroll_accounting",
            "RL environment models quote availability, latency, sizing, and bankroll.",
            &[LearningLaneId::ReinforcementLearning],
            &[
                "latency assumptions",
                "quote availability report",
                "sizing and bankroll rules",
            ],
            "do not claim executable RL performance",
        ),
        gate(
            "dry_run_handoff_packet",
            "Dry-run handoff packet contains model, schema, entry, evidence, and monitoring.",
            &[LearningLaneId::DryRunStrategy],
            &[
                "event_ml_strategy_handoff.json",
                "event_ml_strategy_handoff.md",
                "model metadata",
                "monitoring checklist",
            ],
            "do not deploy a dry-run strategy candidate",
        ),
    ]
}

fn phase(
    id: PhaseId,
    order: u8,
    title: &str,
    goal: &str,
    required_inputs: &[&str],
    required_outputs: &[&str],
    stop_gate: &str,
) -> WorkflowPhase {
    WorkflowPhase {
        id,
        order,
        title: title.to_string(),
        goal: goal.to_string(),
        required_inputs: strings(required_inputs),
        required_outputs: strings(required_outputs),
        stop_gate: stop_gate.to_string(),
    }
}

fn artifact(
    id: &str,
    path_pattern: &str,
    produced_by: PhaseId,
    consumed_by: &[PhaseId],
    description: &str,
) -> ArchitectureArtifact {
    ArchitectureArtifact {
        id: id.to_string(),
        path_pattern: path_pattern.to_string(),
        produced_by,
        consumed_by: consumed_by.to_vec(),
        description: description.to_string(),
    }
}

fn gate(
    id: &str,
    title: &str,
    required_for: &[LearningLaneId],
    evidence: &[&str],
    stop_if_missing: &str,
) -> ReadinessGate {
    ReadinessGate {
        id: id.to_string(),
        title: title.to_string(),
        required_for: required_for.to_vec(),
        evidence: strings(evidence),
        stop_if_missing: stop_if_missing.to_string(),
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

pub fn gate_matrix(architecture: &EventMlArchitecture) -> BTreeMap<String, Vec<String>> {
    architecture
        .learning_lanes
        .iter()
        .map(|lane| {
            (
                lane.id.as_str().to_string(),
                architecture
                    .gates_for_lane(lane.id)
                    .into_iter()
                    .map(|gate| gate.id.clone())
                    .collect(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_phase_order_is_stable() {
        let architecture = canonical_event_ml_architecture();
        assert_eq!(
            architecture.phases.first().map(|phase| phase.id),
            Some(PhaseId::DatasetContract)
        );
        assert_eq!(
            architecture.phases.last().map(|phase| phase.id),
            Some(PhaseId::DryRunHandoff)
        );

        let mut seen = BTreeSet::new();
        for phase in &architecture.phases {
            assert!(seen.insert(phase.id), "duplicate phase id: {:?}", phase.id);
            assert_eq!(architecture.phase(phase.id), Some(phase));
        }
    }

    #[test]
    fn dl_and_rl_are_gated_lanes() {
        let architecture = canonical_event_ml_architecture();
        let dl = architecture
            .learning_lane(LearningLaneId::DeepLearning)
            .expect("DL lane exists");
        let rl = architecture
            .learning_lane(LearningLaneId::ReinforcementLearning)
            .expect("RL lane exists");

        assert_eq!(dl.status, LaneStatus::Gated);
        assert_eq!(rl.status, LaneStatus::Gated);
        assert!(dl.starts_after.contains(&PhaseId::DlGate));
        assert!(rl.starts_after.contains(&PhaseId::RlGate));
    }

    #[test]
    fn rl_readiness_requires_execution_semantics() {
        let architecture = canonical_event_ml_architecture();
        let gate_ids: BTreeSet<_> = architecture
            .gates_for_lane(LearningLaneId::ReinforcementLearning)
            .into_iter()
            .map(|gate| gate.id.as_str())
            .collect();

        assert!(gate_ids.contains("rl_decision_time_state"));
        assert!(gate_ids.contains("rl_action_space"));
        assert!(gate_ids.contains("rl_binary_reward"));
        assert!(gate_ids.contains("rl_latency_bankroll_accounting"));
        assert!(gate_ids.contains("executable_price_accounting"));
    }

    #[test]
    fn readiness_reports_missing_gates() {
        let architecture = canonical_event_ml_architecture();
        let readiness = architecture.evaluate_lane_readiness(
            LearningLaneId::ReinforcementLearning,
            [
                "event_split_disjoint",
                "walk_forward_stability",
                "executable_price_accounting",
            ],
        );

        assert!(!readiness.ready);
        assert!(readiness
            .missing_gate_ids
            .contains(&"rl_binary_reward".to_string()));
    }
}
