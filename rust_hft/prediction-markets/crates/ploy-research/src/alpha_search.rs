use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use hft_search_kernel::{
    backpropagate as kernel_backpropagate, select_expandable as kernel_select_expandable,
    validate_tree as validate_kernel_tree, UctError, UctNode, UctStats,
};

use crate::autofactor::{
    autofactor_runtime_contract_catalog, autofactor_target_contract, autofactor_target_horizon,
    factor_expr_hash, AutoFactorDecision, AutoFactorOptions, AutoFactorReport, FactorExpr,
    LlmPriorSpec,
};
use crate::factors_v2::ReviewSide;

pub const ALPHA_SEARCH_ARTIFACT_VERSION: &str = "alpha_search_artifacts_v1";
pub const SIDE_BOUND_ALPHA_SEARCH_ARTIFACT_VERSION: &str = "alpha_search_artifacts_v2";
pub const FORMULA_MCTS_CHECKPOINT_VERSION: &str = "formula_mcts_checkpoint_v1";
const FORMULA_MCTS_ROOT_ID: &str = "__formula_mcts_root__";
const FORMULA_MCTS_SELECTION_BUDGET: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateReplayFactorIdentity {
    version: String,
    target: String,
    side: Option<ReviewSide>,
    name: String,
    dsl_hash: String,
    horizon: String,
    runtime_score: String,
}

impl CandidateReplayFactorIdentity {
    pub fn from_artifact(artifact: &serde_json::Value) -> Result<Self, AlphaSearchArtifactError> {
        let source = artifact
            .get("source_factor")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| identity_mismatch("candidate replay missing source_factor object"))?;
        let version = required_identity_string(source, "version")?;
        let target = required_identity_string(source, "target")?;
        let name = required_identity_string(source, "name")?;
        let dsl_hash = required_identity_string(source, "dsl_hash")?;
        if dsl_hash.len() != 64
            || !dsl_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(identity_mismatch(
                "candidate replay source_factor dsl_hash must be 64 lowercase hex characters",
            ));
        }
        let horizon = required_identity_string(source, "horizon")?;
        if !source.contains_key("side") {
            return Err(identity_mismatch(
                "candidate replay source_factor missing side",
            ));
        }
        let side = serde_json::from_value::<Option<ReviewSide>>(source["side"].clone())
            .map_err(|_| identity_mismatch("candidate replay source_factor has invalid side"))?;

        let target_contract = autofactor_target_contract(&target).ok_or_else(|| {
            identity_mismatch(format!(
                "candidate replay source_factor has unsupported target={target}"
            ))
        })?;
        if is_side_bound_repricing_target(&target) {
            if version != SIDE_BOUND_ALPHA_SEARCH_ARTIFACT_VERSION || side.is_none() {
                return Err(identity_mismatch(format!(
                    "repricing source_factor requires version={SIDE_BOUND_ALPHA_SEARCH_ARTIFACT_VERSION} and side=Up|Down"
                )));
            }
        } else if target_contract.official_settlement_required {
            if version != ALPHA_SEARCH_ARTIFACT_VERSION || side.is_some() {
                return Err(identity_mismatch(format!(
                    "settlement source_factor requires version={ALPHA_SEARCH_ARTIFACT_VERSION} and side=null"
                )));
            }
        } else {
            return Err(identity_mismatch(format!(
                "candidate replay source_factor has unsupported target={target}"
            )));
        }

        let expected_horizon = target_contract.horizon.as_str();
        if horizon != expected_horizon {
            return Err(identity_mismatch(format!(
                "candidate replay source_factor target={target} requires horizon={expected_horizon}, found {horizon}"
            )));
        }
        for (field, expected) in [
            ("version", version.as_str()),
            ("target", target.as_str()),
            ("name", name.as_str()),
            ("dsl_hash", dsl_hash.as_str()),
            ("horizon", horizon.as_str()),
        ] {
            validate_duplicate_identity_string("top-level", artifact, field, expected)?;
        }
        validate_duplicate_identity_side("top-level", artifact, side)?;
        if let Some(decision_contract) = artifact.get("decision_contract") {
            if !decision_contract.is_object() {
                return Err(identity_mismatch(
                    "candidate replay decision_contract must be an object",
                ));
            }
            for (field, expected) in [("target", target.as_str()), ("horizon", horizon.as_str())] {
                validate_duplicate_identity_string(
                    "decision_contract",
                    decision_contract,
                    field,
                    expected,
                )?;
            }
            validate_duplicate_identity_side("decision_contract", decision_contract, side)?;
        }

        let runtime_score = artifact
            .get("runtime_score")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| identity_mismatch("candidate replay missing runtime_score"))?
            .to_string();
        let expected_runtime_score = inferred_runtime_mapping(&name).runtime_score;
        if expected_runtime_score.is_empty() {
            return Err(identity_mismatch(format!(
                "candidate replay source_factor has no canonical runtime score for name={name}"
            )));
        }
        if runtime_score != expected_runtime_score {
            return Err(identity_mismatch(format!(
                "candidate replay runtime_score must be {expected_runtime_score}, found {runtime_score}"
            )));
        }

        Ok(Self {
            version,
            target,
            side,
            name,
            dsl_hash,
            horizon,
            runtime_score,
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn side(&self) -> Option<ReviewSide> {
        self.side
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn dsl_hash(&self) -> &str {
        &self.dsl_hash
    }

    pub fn horizon(&self) -> &str {
        &self.horizon
    }

    pub fn runtime_score(&self) -> &str {
        &self.runtime_score
    }
}

fn identity_mismatch(reason: impl Into<String>) -> AlphaSearchArtifactError {
    AlphaSearchArtifactError::IdentityMismatch(reason.into())
}

fn required_identity_string(
    source: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String, AlphaSearchArtifactError> {
    source
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            identity_mismatch(format!(
                "candidate replay source_factor missing non-empty {field}"
            ))
        })
}

fn validate_duplicate_identity_string(
    location: &str,
    artifact: &serde_json::Value,
    field: &str,
    expected: &str,
) -> Result<(), AlphaSearchArtifactError> {
    if let Some(found) = artifact.get(field) {
        if found.as_str() != Some(expected) {
            return Err(identity_mismatch(format!(
                "candidate replay {location} {field} must match source_factor {expected}"
            )));
        }
    }
    Ok(())
}

fn validate_duplicate_identity_side(
    location: &str,
    artifact: &serde_json::Value,
    expected: Option<ReviewSide>,
) -> Result<(), AlphaSearchArtifactError> {
    let Some(found) = artifact.get("side") else {
        return Ok(());
    };
    let found = serde_json::from_value::<Option<ReviewSide>>(found.clone())
        .map_err(|_| identity_mismatch(format!("candidate replay {location} side is invalid")))?;
    if found != expected {
        return Err(identity_mismatch(format!(
            "candidate replay {location} side must match source_factor"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct AlphaSearchArtifactSummary {
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<ReviewSide>,
    pub output_dir: String,
    pub candidate_count: usize,
    pub rejected_count: usize,
    pub best_candidate: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlphaSearchRuntimeFeedback {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<ReviewSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsl_hash: Option<String>,
    pub runtime_score: String,
    pub base_factor: String,
    pub entry_signals: usize,
    pub direct_passes_at_configured_threshold: usize,
    pub formula_evaluations: usize,
    pub depth_fillable: usize,
    pub executable_edge_pass_min_edge: usize,
}

impl AlphaSearchRuntimeFeedback {
    pub fn is_pass_through_collapse(&self) -> bool {
        (self.direct_passes_at_configured_threshold >= 50 && self.entry_signals < 50)
            || (self.formula_evaluations >= 500 && self.executable_edge_pass_min_edge < 50)
            || (self.depth_fillable >= 500 && self.entry_signals < 50)
    }

    fn entry_signal_rate(&self) -> f64 {
        ratio_usize(
            self.entry_signals,
            self.direct_passes_at_configured_threshold,
        )
    }

    fn executable_edge_pass_rate(&self) -> f64 {
        ratio_usize(self.executable_edge_pass_min_edge, self.formula_evaluations)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MctsSearchStateArtifact {
    pub version: String,
    pub mode: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<ReviewSide>,
    pub total_visits: usize,
    #[serde(default)]
    pub backpropagation_truncated_count: usize,
    pub nodes: Vec<MctsSearchStateNode>,
    #[serde(default)]
    pub subtree_frequencies: Vec<SubtreeFrequencyState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MctsSearchStateNode {
    pub factor_name: String,
    #[serde(default)]
    pub parent_name: Option<String>,
    pub visits: usize,
    pub total_reward: f64,
    pub best_reward: f64,
    pub last_reward: f64,
    pub selected_dimension: String,
    pub last_decision: String,
}

/// The only mutable Formula search state. It binds Formula-specific candidate
/// identity and metadata around the domain-neutral shared UCT kernel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormulaMctsCheckpoint {
    pub version: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<ReviewSide>,
    pub selection_budget: usize,
    pub nodes: Vec<FormulaMctsCheckpointNode>,
    #[serde(default)]
    pub subtree_frequencies: Vec<SubtreeFrequencyState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormulaMctsCheckpointNode {
    pub factor_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_name: Option<String>,
    pub visits: u64,
    pub total_reward: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_reward: Option<f64>,
    pub last_reward: f64,
    pub selected_dimension: String,
    pub last_decision: String,
}

/// A durable, cross-run snapshot of previously-accepted factors grouped by
/// root gene, sourced from the `factor_registry` table across all historical
/// runs. This is the "Alpha Zoo" from the "Navigating the Alpha Jungle" paper:
/// unlike Frequent-Subtree Avoidance (batch-local, current run only), this
/// snapshot represents the full historical population a new candidate should
/// be checked against for novelty.
///
/// Absence of a snapshot (`None`) must always mean "no penalty, no effect",
/// mirroring how `AlphaSearchRuntimeFeedback` and `LlmPriorSpec` behave when
/// not supplied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlphaZooSnapshot {
    pub version: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<ReviewSide>,
    pub entries: Vec<AlphaZooEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlphaZooEntry {
    pub root_gene: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtreeFrequencyState {
    pub root_gene: String,
    pub structural_signature: String,
    pub depth: usize,
    pub count: usize,
}

#[derive(Debug)]
pub enum AlphaSearchArtifactError {
    Io(std::io::Error),
    Json(serde_json::Error),
    IdentityMismatch(String),
    LegacyCheckpointVersion(String),
    Kernel(UctError),
}

impl fmt::Display for AlphaSearchArtifactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "alpha search artifact I/O failed: {err}"),
            Self::Json(err) => write!(f, "alpha search artifact JSON failed: {err}"),
            Self::IdentityMismatch(reason) => {
                write!(f, "alpha search artifact identity mismatch: {reason}")
            }
            Self::LegacyCheckpointVersion(version) => write!(
                f,
                "alpha search checkpoint version `{version}` is legacy; expected `{FORMULA_MCTS_CHECKPOINT_VERSION}`"
            ),
            Self::Kernel(err) => write!(f, "alpha search shared MCTS kernel failed: {err}"),
        }
    }
}

impl std::error::Error for AlphaSearchArtifactError {}

impl From<std::io::Error> for AlphaSearchArtifactError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for AlphaSearchArtifactError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<UctError> for AlphaSearchArtifactError {
    fn from(value: UctError) -> Self {
        Self::Kernel(value)
    }
}

#[derive(Debug, Serialize)]
struct SearchSpaceArtifact {
    version: &'static str,
    mode: &'static str,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    feature_pool: Vec<String>,
    constant_pool: Vec<f64>,
    operator_pool: Vec<&'static str>,
    limits: SearchLimits,
}

#[derive(Debug, Serialize)]
struct SearchLimits {
    min_observations: usize,
    min_window_observations: usize,
    bucket_count: usize,
    min_spearman_ic: f64,
    min_icir: f64,
    min_positive_window_ratio: f64,
    min_top_bucket_avg_label: f64,
    min_monotonicity_score: f64,
    max_complexity: usize,
}

#[derive(Debug, Serialize)]
struct LlmPriorArtifact {
    version: &'static str,
    mode: &'static str,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    hypotheses: Vec<PriorHypothesis>,
    allowed_mutation_types: Vec<&'static str>,
    note: &'static str,
}

#[derive(Debug, Serialize)]
struct PriorHypothesis {
    id: &'static str,
    hypothesis: &'static str,
    expected_mechanism: &'static str,
    required_surfaces: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct CandidateExpression {
    name: String,
    target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    source: &'static str,
    complexity: usize,
    root_gene: String,
    structural_signature: String,
    expr: FactorExpr,
}

#[derive(Debug, Serialize)]
struct RejectedExpression {
    name: String,
    target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    root_gene: String,
    structural_signature: String,
    reason: String,
    complexity: usize,
}

#[derive(Debug, Serialize)]
struct TreeTraceArtifact {
    version: &'static str,
    mode: &'static str,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    nodes: Vec<TreeTraceNode>,
}

#[derive(Debug, Serialize)]
struct TreeTraceNode {
    id: String,
    parent: Option<String>,
    factor_name: String,
    mutation: &'static str,
    selected_dimension: String,
    reward: f64,
    visits: usize,
    decision: String,
}

#[derive(Debug, Serialize)]
struct NodeMetric {
    id: String,
    factor_name: String,
    parent_name: Option<String>,
    target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    decision: String,
    reason: String,
    selected_dimension: String,
    effectiveness: f64,
    stability: f64,
    diversity: f64,
    alpha_zoo_novelty: f64,
    simplicity: f64,
    structural_novelty: f64,
    diversity_penalty: f64,
    execution_cost: f64,
    event_uniqueness: f64,
    overfit_risk: f64,
    runtime_readiness: f64,
    reward: f64,
    spearman_ic: f64,
    icir: f64,
    positive_window_ratio: f64,
    top_bucket_avg_label: f64,
    top_bucket_full_depth_entry_fill_rate: f64,
    top_bucket_avg_entry_sweep_slippage_bps: f64,
    top_bucket_avg_entry_sweep_levels: f64,
    top_bucket_unique_event_count: usize,
    top_bucket_max_event_decisions: usize,
    runtime_entry_signal_rate: Option<f64>,
    runtime_executable_edge_pass_rate: Option<f64>,
    runtime_pass_through_penalty: f64,
    alpha_zoo_penalty: f64,
    monotonicity_score: f64,
    complexity: usize,
}

#[derive(Debug, Serialize)]
struct AvoidedSubtree {
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    root_gene: String,
    structural_signature: String,
    depth: usize,
    count: usize,
    action: &'static str,
    reason: &'static str,
}

#[derive(Debug, Serialize)]
struct SearchFeedbackArtifact {
    version: &'static str,
    mode: &'static str,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    candidate_count: usize,
    rejected_count: usize,
    watchlist_count: usize,
    passed_count: usize,
    best_candidate: Option<String>,
    best_reward: Option<f64>,
    runtime_feedback: Option<RuntimeFeedbackSummary>,
    runtime_avoid_factors: Vec<RuntimeAvoidFactorSummary>,
    interpretation: &'static str,
}

#[derive(Debug, Serialize)]
struct RuntimeFeedbackSummary {
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    runtime_score: String,
    base_factor: String,
    entry_signals: usize,
    direct_passes_at_configured_threshold: usize,
    formula_evaluations: usize,
    executable_edge_pass_min_edge: usize,
    pass_through_collapse: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeAvoidFactorSummary {
    base_factor: String,
    factor_family: String,
    runtime_score: Option<String>,
    reason: Option<String>,
    source: &'static str,
}

#[derive(Debug, Clone)]
struct RuntimeAvoidance {
    base_factor: String,
    factor_family: String,
    dsl_hash: Option<String>,
    runtime_score: Option<String>,
    reason: Option<String>,
    source: &'static str,
    entry_signal_rate: Option<f64>,
    executable_edge_pass_rate: Option<f64>,
    penalty: f64,
}

#[derive(Debug, Serialize)]
struct FactorRegistryPreviewRow {
    factor_name: String,
    target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    horizon: String,
    dsl_hash: String,
    ast_json: serde_json::Value,
    runtime_contract: serde_json::Value,
    status: &'static str,
    metrics: serde_json::Value,
    blockers: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FactorRegistryPreviewArtifact {
    version: &'static str,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    horizon: String,
    factors: Vec<FactorRegistryPreviewRow>,
}

#[derive(Debug, Serialize)]
struct RuntimeInputMapping {
    ast_input_name: String,
    runtime_input_names: Vec<String>,
    projection: String,
}

#[derive(Debug)]
struct RuntimeInputProjection {
    runtime_input_names: Vec<String>,
    mappings: Vec<RuntimeInputMapping>,
    blockers: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MctsExpansionPlan {
    version: &'static str,
    mode: &'static str,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReviewSide>,
    exploration_weight: f64,
    selected_nodes: Vec<MctsSelectedNode>,
    note: &'static str,
}

#[derive(Debug, Serialize)]
struct MctsSelectedNode {
    node_id: String,
    factor_name: String,
    selected_dimension: String,
    proposed_mutation: &'static str,
    reward: f64,
}

pub fn write_alpha_search_artifacts(
    output_root: impl AsRef<Path>,
    target: &str,
    input_names: &[String],
    reports: &[AutoFactorReport],
    options: &AutoFactorOptions,
) -> Result<AlphaSearchArtifactSummary, AlphaSearchArtifactError> {
    write_alpha_search_artifacts_with_state(
        output_root,
        target,
        input_names,
        reports,
        options,
        None,
    )
}

pub fn read_mcts_search_state(
    path: impl AsRef<Path>,
) -> Result<MctsSearchStateArtifact, AlphaSearchArtifactError> {
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

pub fn read_formula_mcts_checkpoint(
    path: impl AsRef<Path>,
) -> Result<FormulaMctsCheckpoint, AlphaSearchArtifactError> {
    let raw = std::fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<missing>");
    if version != FORMULA_MCTS_CHECKPOINT_VERSION {
        return Err(AlphaSearchArtifactError::LegacyCheckpointVersion(
            version.to_string(),
        ));
    }
    let checkpoint = serde_json::from_value::<FormulaMctsCheckpoint>(value)?;
    validate_formula_mcts_checkpoint(&checkpoint)?;
    Ok(checkpoint)
}

pub fn write_alpha_search_artifacts_with_state(
    output_root: impl AsRef<Path>,
    target: &str,
    input_names: &[String],
    reports: &[AutoFactorReport],
    options: &AutoFactorOptions,
    prior_state: Option<&FormulaMctsCheckpoint>,
) -> Result<AlphaSearchArtifactSummary, AlphaSearchArtifactError> {
    write_alpha_search_artifacts_with_state_and_runtime_feedback(
        output_root,
        target,
        input_names,
        reports,
        options,
        prior_state,
        None,
        None,
        None,
    )
}

// Keep the stable artifact-writer API explicit; grouping these optional evidence
// inputs would be a breaking change for existing research callers.
#[allow(clippy::too_many_arguments)]
pub fn write_alpha_search_artifacts_with_state_and_runtime_feedback(
    output_root: impl AsRef<Path>,
    target: &str,
    input_names: &[String],
    reports: &[AutoFactorReport],
    options: &AutoFactorOptions,
    prior_state: Option<&FormulaMctsCheckpoint>,
    runtime_feedback: Option<&AlphaSearchRuntimeFeedback>,
    llm_prior: Option<&LlmPriorSpec>,
    alpha_zoo: Option<&AlphaZooSnapshot>,
) -> Result<AlphaSearchArtifactSummary, AlphaSearchArtifactError> {
    if is_side_bound_repricing_target(target)
        || reports.iter().any(|report| report.side.is_some())
        || prior_state.is_some_and(|state| state.side.is_some())
        || runtime_feedback.is_some_and(|feedback| {
            feedback.version.as_deref() == Some(SIDE_BOUND_ALPHA_SEARCH_ARTIFACT_VERSION)
                || feedback.side.is_some()
        })
        || alpha_zoo.is_some_and(|zoo| {
            zoo.version == SIDE_BOUND_ALPHA_SEARCH_ARTIFACT_VERSION || zoo.side.is_some()
        })
    {
        return Err(AlphaSearchArtifactError::IdentityMismatch(
            "side-bound repricing inputs require the side-bound writer".to_string(),
        ));
    }
    write_alpha_search_artifacts_core(
        output_root,
        target,
        None,
        input_names,
        reports,
        options,
        prior_state,
        runtime_feedback,
        llm_prior,
        alpha_zoo,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn write_side_bound_alpha_search_artifacts_with_state_and_runtime_feedback(
    output_root: impl AsRef<Path>,
    target: &str,
    side: ReviewSide,
    input_names: &[String],
    reports: &[AutoFactorReport],
    options: &AutoFactorOptions,
    prior_state: Option<&FormulaMctsCheckpoint>,
    runtime_feedback: Option<&AlphaSearchRuntimeFeedback>,
    alpha_zoo: Option<&AlphaZooSnapshot>,
) -> Result<AlphaSearchArtifactSummary, AlphaSearchArtifactError> {
    write_alpha_search_artifacts_core(
        output_root,
        target,
        Some(side),
        input_names,
        reports,
        options,
        prior_state,
        runtime_feedback,
        None,
        alpha_zoo,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_alpha_search_artifacts_core(
    output_root: impl AsRef<Path>,
    target: &str,
    side: Option<ReviewSide>,
    input_names: &[String],
    reports: &[AutoFactorReport],
    options: &AutoFactorOptions,
    prior_state: Option<&FormulaMctsCheckpoint>,
    runtime_feedback: Option<&AlphaSearchRuntimeFeedback>,
    llm_prior: Option<&LlmPriorSpec>,
    alpha_zoo: Option<&AlphaZooSnapshot>,
) -> Result<AlphaSearchArtifactSummary, AlphaSearchArtifactError> {
    if let Some(side) = side {
        validate_side_bound_inputs(
            target,
            side,
            reports,
            prior_state,
            runtime_feedback,
            alpha_zoo,
        )?;
    }
    let version = if side.is_some() {
        SIDE_BOUND_ALPHA_SEARCH_ARTIFACT_VERSION
    } else {
        ALPHA_SEARCH_ARTIFACT_VERSION
    };
    let output_dir = side.map_or_else(
        || output_root.as_ref().join(target),
        |side| output_root.as_ref().join(target).join(side.as_str()),
    );
    std::fs::create_dir_all(&output_dir)?;

    let feature_pool = {
        let mut values = input_names.to_vec();
        values.sort();
        values
    };
    write_json(
        &output_dir.join("search-space.json"),
        &SearchSpaceArtifact {
            version,
            mode: "deterministic_seed_search",
            target: target.to_string(),
            side,
            feature_pool,
            constant_pool: vec![
                0.001, 0.005, 0.01, 0.02, 0.05, 0.10, 1.0, 2.0, 3.0, 5.0, 10.0, 30.0, 60.0, 300.0,
            ],
            operator_pool: vec![
                "Input",
                "Const",
                "Add",
                "Sub",
                "Mul",
                "SafeDiv",
                "Max",
                "Min",
                "Tanh",
                "Log1pAbs",
                "SqrtAbs",
                "Clip",
                "Delta",
                "RollingMean",
                "RollingStd",
                "ZScore",
                "Gate",
            ],
            limits: SearchLimits {
                min_observations: options.min_observations,
                min_window_observations: options.min_window_observations,
                bucket_count: options.bucket_count,
                min_spearman_ic: options.min_spearman_ic,
                min_icir: options.min_icir,
                min_positive_window_ratio: options.min_positive_window_ratio,
                min_top_bucket_avg_label: options.min_top_bucket_avg_label,
                min_monotonicity_score: options.min_monotonicity_score,
                max_complexity: options.max_complexity,
            },
        },
    )?;

    write_json(
        &output_dir.join("llm-priors.json"),
        &LlmPriorArtifact {
            version,
            mode: "deterministic_domain_prior_placeholder",
            target: target.to_string(),
            side,
            hypotheses: default_hypotheses(target),
            allowed_mutation_types: vec![
                "add_feature_gate",
                "replace_denominator",
                "add_spread_penalty",
                "add_capacity_gate",
                "add_near_strike_interaction",
                "change_time_window",
                "clip_or_squash",
                "invert_or_contrarian",
                "remove_component",
            ],
            note: "External LLM expansion is not invoked in this artifact. This file records the machine-checkable prior schema used by deterministic seed search.",
        },
    )?;

    let candidates = reports
        .iter()
        .map(|report| CandidateExpression {
            name: report.name.clone(),
            target: report.target.clone(),
            side: report.side,
            source: candidate_source(&report.name),
            complexity: report.complexity,
            root_gene: root_gene(&report.expr),
            structural_signature: structural_signature(&report.expr),
            expr: report.expr.clone(),
        })
        .collect::<Vec<_>>();
    write_json(&output_dir.join("candidate-expressions.json"), &candidates)?;

    let rejected = reports
        .iter()
        .filter(|report| report.decision == AutoFactorDecision::Reject)
        .map(|report| RejectedExpression {
            name: report.name.clone(),
            target: report.target.clone(),
            side: report.side,
            root_gene: root_gene(&report.expr),
            structural_signature: structural_signature(&report.expr),
            reason: report.reason.clone(),
            complexity: report.complexity,
        })
        .collect::<Vec<_>>();
    write_json(&output_dir.join("rejected-expressions.json"), &rejected)?;

    let runtime_avoidances = runtime_avoidances(runtime_feedback, llm_prior);
    let subtree_frequencies = subtree_frequency_state(target, reports, prior_state, llm_prior);
    let node_metrics = reports
        .iter()
        .enumerate()
        .map(|(idx, report)| {
            node_metric(
                idx,
                report,
                &runtime_avoidances,
                alpha_zoo,
                &subtree_frequencies,
            )
        })
        .collect::<Vec<_>>();
    write_json(&output_dir.join("node-metrics.json"), &node_metrics)?;
    write_json(
        &output_dir.join("factor-registry-preview.json"),
        &factor_registry_preview_artifact(version, target, side, reports, &node_metrics)?,
    )?;
    let checkpoint = formula_mcts_checkpoint(
        version,
        target,
        side,
        &node_metrics,
        prior_state,
        subtree_frequencies,
    )?;
    let mcts_state = mcts_search_state_projection(&checkpoint)?;
    write_json(
        &output_dir.join("formula-mcts-checkpoint.json"),
        &checkpoint,
    )?;
    write_json(&output_dir.join("mcts-state.json"), &mcts_state)?;
    write_json(
        &output_dir.join("mcts-expansion-plan.json"),
        &formula_mcts_expansion_plan(version, target, side, &checkpoint)?,
    )?;

    write_json(
        &output_dir.join("tree-trace.json"),
        &TreeTraceArtifact {
            version,
            mode: "single_depth_seed_tree",
            target: target.to_string(),
            side,
            nodes: reports
                .iter()
                .enumerate()
                .map(|(idx, report)| TreeTraceNode {
                    id: format!("node-{idx}"),
                    parent: report.parent_name.clone(),
                    factor_name: report.name.clone(),
                    mutation: "seed",
                    selected_dimension: selected_dimension(report, &runtime_avoidances),
                    reward: reward(
                        report,
                        &runtime_avoidances,
                        alpha_zoo,
                        &checkpoint.subtree_frequencies,
                    ),
                    visits: 1,
                    decision: report.decision.as_str().to_string(),
                })
                .collect(),
        },
    )?;

    write_json(
        &output_dir.join("avoided-subtrees.json"),
        &avoided_subtrees(side, &mcts_state.subtree_frequencies),
    )?;

    let best = node_metrics
        .iter()
        .max_by(|lhs, rhs| lhs.reward.total_cmp(&rhs.reward));
    let feedback = SearchFeedbackArtifact {
        version,
        mode: "deterministic_seed_search",
        target: target.to_string(),
        side,
        candidate_count: reports.len(),
        rejected_count: rejected.len(),
        watchlist_count: reports
            .iter()
            .filter(|report| report.decision == AutoFactorDecision::Watchlist)
            .count(),
        passed_count: reports
            .iter()
            .filter(|report| report.decision == AutoFactorDecision::Candidate)
            .count(),
        best_candidate: best.map(|metric| metric.factor_name.clone()),
        best_reward: best.map(|metric| metric.reward),
        runtime_feedback: runtime_feedback.map(|feedback| RuntimeFeedbackSummary {
            side: feedback.side,
            runtime_score: feedback.runtime_score.clone(),
            base_factor: feedback.base_factor.clone(),
            entry_signals: feedback.entry_signals,
            direct_passes_at_configured_threshold: feedback.direct_passes_at_configured_threshold,
            formula_evaluations: feedback.formula_evaluations,
            executable_edge_pass_min_edge: feedback.executable_edge_pass_min_edge,
            pass_through_collapse: feedback.is_pass_through_collapse(),
        }),
        runtime_avoid_factors: runtime_avoidances
            .iter()
            .map(|avoidance| RuntimeAvoidFactorSummary {
                base_factor: avoidance.base_factor.clone(),
                factor_family: avoidance.factor_family.clone(),
                runtime_score: avoidance.runtime_score.clone(),
                reason: avoidance.reason.clone(),
                source: avoidance.source,
            })
            .collect(),
        interpretation: "Search feedback is discovery evidence only. Promotion still requires the AutoFactor strategy-promotion gate and replay/runtime parity.",
    };
    write_json(&output_dir.join("search-feedback.json"), &feedback)?;

    Ok(AlphaSearchArtifactSummary {
        target: target.to_string(),
        side,
        output_dir: output_dir.display().to_string(),
        candidate_count: reports.len(),
        rejected_count: rejected.len(),
        best_candidate: feedback.best_candidate,
    })
}

fn validate_side_bound_inputs(
    target: &str,
    side: ReviewSide,
    reports: &[AutoFactorReport],
    prior_state: Option<&FormulaMctsCheckpoint>,
    runtime_feedback: Option<&AlphaSearchRuntimeFeedback>,
    alpha_zoo: Option<&AlphaZooSnapshot>,
) -> Result<(), AlphaSearchArtifactError> {
    if !is_side_bound_repricing_target(target) {
        return Err(AlphaSearchArtifactError::IdentityMismatch(format!(
            "side-bound writer does not support target={target}"
        )));
    }
    for report in reports {
        if report.target.as_deref() != Some(target) || report.side != Some(side) {
            return Err(AlphaSearchArtifactError::IdentityMismatch(format!(
                "report `{}` expected target={target} side={}, found target={} side={}",
                report.name,
                side.as_str(),
                report.target.as_deref().unwrap_or("<missing>"),
                report.side.map(ReviewSide::as_str).unwrap_or("<missing>")
            )));
        }
    }
    if let Some(state) = prior_state {
        if state.version != FORMULA_MCTS_CHECKPOINT_VERSION
            || state.target != target
            || state.side != Some(side)
        {
            return Err(AlphaSearchArtifactError::IdentityMismatch(format!(
                "Formula MCTS checkpoint expected target={target} side={}, found version={} target={} side={}",
                side.as_str(),
                state.version,
                state.target,
                state.side.map(ReviewSide::as_str).unwrap_or("<missing>")
            )));
        }
    }
    if let Some(feedback) = runtime_feedback {
        validate_side_bound_identity(
            "runtime feedback",
            feedback.version.as_deref(),
            feedback.target.as_deref(),
            feedback.side,
            target,
            side,
        )?;
    }
    if let Some(zoo) = alpha_zoo {
        validate_side_bound_identity(
            "Alpha Zoo",
            Some(zoo.version.as_str()),
            Some(zoo.target.as_str()),
            zoo.side,
            target,
            side,
        )?;
    }
    Ok(())
}

fn is_side_bound_repricing_target(target: &str) -> bool {
    matches!(
        target,
        "full_depth_reprice_pnl_10s" | "full_depth_reprice_pnl_15s" | "full_depth_reprice_pnl_30s"
    )
}

fn validate_side_bound_identity(
    kind: &str,
    version: Option<&str>,
    target: Option<&str>,
    side: Option<ReviewSide>,
    expected_target: &str,
    expected_side: ReviewSide,
) -> Result<(), AlphaSearchArtifactError> {
    if version != Some(SIDE_BOUND_ALPHA_SEARCH_ARTIFACT_VERSION)
        || target != Some(expected_target)
        || side != Some(expected_side)
    {
        return Err(AlphaSearchArtifactError::IdentityMismatch(format!(
            "{kind} expected version={SIDE_BOUND_ALPHA_SEARCH_ARTIFACT_VERSION} target={expected_target} side={}, found version={} target={} side={}",
            expected_side.as_str(),
            version.unwrap_or("<missing>"),
            target.unwrap_or("<missing>"),
            side.map(ReviewSide::as_str).unwrap_or("<missing>")
        )));
    }
    Ok(())
}

fn factor_registry_preview_rows(
    target: &str,
    reports: &[AutoFactorReport],
    node_metrics: &[NodeMetric],
) -> Result<Vec<FactorRegistryPreviewRow>, AlphaSearchArtifactError> {
    let horizon = factor_horizon(target);
    reports
        .iter()
        .zip(node_metrics.iter())
        .map(|(report, metric)| {
            let dsl_hash = factor_expr_hash(&report.expr)?;
            let ast_json = serde_json::to_value(&report.expr)?;
            let runtime_contract =
                runtime_contract_for_report(report, &dsl_hash, &ast_json, &horizon);
            let blockers = registry_blockers(report, &runtime_contract);
            Ok(FactorRegistryPreviewRow {
                factor_name: report.name.clone(),
                target: report.target.clone(),
                side: report.side,
                horizon: horizon.clone(),
                dsl_hash,
                ast_json,
                runtime_contract,
                status: registry_status(report.decision),
                metrics: serde_json::to_value(metric)?,
                blockers,
            })
        })
        .collect()
}

fn factor_registry_preview_artifact(
    version: &'static str,
    target: &str,
    side: Option<ReviewSide>,
    reports: &[AutoFactorReport],
    node_metrics: &[NodeMetric],
) -> Result<FactorRegistryPreviewArtifact, AlphaSearchArtifactError> {
    Ok(FactorRegistryPreviewArtifact {
        version,
        target: target.to_string(),
        side,
        horizon: factor_horizon(target),
        factors: factor_registry_preview_rows(target, reports, node_metrics)?,
    })
}

fn registry_status(decision: AutoFactorDecision) -> &'static str {
    match decision {
        AutoFactorDecision::Candidate => "candidate",
        AutoFactorDecision::Watchlist => "watchlist",
        AutoFactorDecision::Reject => "rejected",
    }
}

fn registry_blockers(
    report: &AutoFactorReport,
    runtime_contract: &serde_json::Value,
) -> Vec<String> {
    let mut blockers = Vec::new();
    if report.decision != AutoFactorDecision::Candidate {
        blockers.push(report.reason.clone());
    }
    if let Some(items) = runtime_contract
        .get("blockers")
        .and_then(serde_json::Value::as_array)
    {
        blockers.extend(
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned),
        );
    }
    blockers.sort();
    blockers.dedup();
    blockers
}

fn factor_horizon(target: &str) -> String {
    autofactor_target_horizon(target).to_string()
}

fn runtime_contract_for_report(
    report: &AutoFactorReport,
    dsl_hash: &str,
    ast_json: &serde_json::Value,
    horizon: &str,
) -> serde_json::Value {
    let ast_input_names = factor_input_names(&report.expr);
    let input_projection = runtime_input_projection(&ast_input_names);
    let mut blockers = Vec::new();
    let mapping = inferred_runtime_mapping(&report.name);
    if mapping.runtime_score.is_empty() || mapping.strategy_profile.is_empty() {
        blockers.push("runtime_contract_unmapped_factor".to_string());
    }
    blockers.extend(input_projection.blockers.clone());
    blockers.extend(runtime_formula_blockers(&report.name));
    blockers.sort();
    blockers.dedup();
    serde_json::json!({
        "version": "autofactor_runtime_contract_v1",
        "dsl_hash": dsl_hash,
        "ast_json": ast_json,
        "runtime_score": mapping.runtime_score,
        "strategy_profile": mapping.strategy_profile,
        "strategy_family": mapping.strategy_family,
        "factor_family": normalized_factor_family(&report.name),
        "target": report.target.as_deref().unwrap_or("unknown"),
        "horizon": horizon,
        "input_names": ast_input_names,
        "ast_input_names": ast_input_names,
        "runtime_input_names": input_projection.runtime_input_names,
        "input_mappings": input_projection.mappings,
        "blockers": blockers,
    })
}

#[derive(Debug, Default)]
struct RuntimeMapping {
    strategy_profile: String,
    strategy_family: String,
    runtime_score: String,
}

fn inferred_runtime_mapping(name: &str) -> RuntimeMapping {
    let normalized = normalized_factor_key(name);
    if normalized == "spread_adjusted_external_move" {
        return RuntimeMapping {
            strategy_profile: "repricing_momentum".to_string(),
            strategy_family: "repricing".to_string(),
            runtime_score: "spread_adjusted_external_move_score".to_string(),
        };
    }
    if normalized == "repricing_gap_side_10s" {
        return RuntimeMapping {
            strategy_profile: "repricing_momentum".to_string(),
            strategy_family: "repricing".to_string(),
            runtime_score: "repricing_gap_side_10s".to_string(),
        };
    }
    if is_settlement_formula(&normalized) {
        return RuntimeMapping {
            strategy_profile: "settlement_probability".to_string(),
            strategy_family: "settlement_probability".to_string(),
            runtime_score: format!("autofactor_formula:{name}"),
        };
    }
    if is_predictive_settlement_formula(&normalized) {
        return RuntimeMapping {
            strategy_profile: "settlement_probability".to_string(),
            strategy_family: "predictive_settlement_probability".to_string(),
            runtime_score: format!("autofactor_formula:{name}"),
        };
    }
    RuntimeMapping::default()
}

fn runtime_input_projection(ast_input_names: &[String]) -> RuntimeInputProjection {
    let mut runtime_inputs = BTreeSet::new();
    let mut mappings = Vec::new();
    let mut blockers = Vec::new();
    for input in ast_input_names {
        match runtime_input_mapping(input) {
            Ok((runtime_input_names, projection)) => {
                for runtime_input in &runtime_input_names {
                    runtime_inputs.insert(runtime_input.clone());
                }
                mappings.push(RuntimeInputMapping {
                    ast_input_name: input.clone(),
                    runtime_input_names,
                    projection,
                });
            }
            Err(blocker) => blockers.push(blocker),
        }
    }
    blockers.sort();
    blockers.dedup();
    RuntimeInputProjection {
        runtime_input_names: runtime_inputs.into_iter().collect(),
        mappings,
        blockers,
    }
}

fn runtime_input_mapping(input: &str) -> Result<(Vec<String>, String), String> {
    let Some(contract) = autofactor_runtime_contract_catalog()
        .research_input_mappings
        .get(input)
    else {
        return Err(format!("runtime_input_unsupported:{input}"));
    };
    if let Some(blocker) = contract.blocker.as_deref() {
        return Err(blocker.to_string());
    }
    if contract.runtime_input_names.is_empty() {
        return Err(format!("runtime_input_unsupported:{input}"));
    }
    Ok((
        contract.runtime_input_names.clone(),
        contract
            .projection
            .clone()
            .unwrap_or_else(|| "runtime_native_input".to_string()),
    ))
}

fn runtime_formula_blockers(name: &str) -> Vec<String> {
    let normalized = normalized_factor_key(name);
    let mut blockers = Vec::new();
    for rule in &autofactor_runtime_contract_catalog().formula_blockers {
        let matches = match rule.match_kind.as_str() {
            "prefix" => normalized.starts_with(&rule.value),
            "contains" => normalized.contains(&rule.value),
            _ => false,
        };
        if matches {
            blockers.push(rule.blocker.clone());
        }
    }
    if is_predictive_formula_base(&normalized) && !is_predictive_settlement_formula(&normalized) {
        blockers.push("runtime_contract_unsupported_predictive_suffix".to_string());
    }
    blockers
}

fn is_predictive_formula_base(normalized: &str) -> bool {
    [
        "amplitude_weighted_momentum_30s_sigma",
        "poly_lag_pressure",
        "spread_adjusted_external_move",
    ]
    .iter()
    .any(|base| normalized.starts_with(base))
}

fn is_predictive_settlement_formula(normalized: &str) -> bool {
    let Some(base) = [
        "amplitude_weighted_momentum_30s_sigma",
        "poly_lag_pressure",
        "spread_adjusted_external_move",
    ]
    .iter()
    .find(|base| normalized.starts_with(**base)) else {
        return false;
    };
    let suffix = normalized.strip_prefix(base).unwrap_or("");
    predictive_formula_suffix_supported(suffix)
}

fn predictive_formula_suffix_supported(suffix: &str) -> bool {
    let suffix = normalize_runtime_formula_suffix(suffix);
    let Some(suffix) = strip_runtime_selector_gates(&suffix) else {
        return false;
    };
    if suffix.is_empty() {
        return true;
    }
    for token in suffix.trim_start_matches('_').split('_') {
        if !matches!(
            token,
            "squashed"
                | "near"
                | "strike"
                | "capacity"
                | "full"
                | "depth"
                | "entry"
                | "gate"
                | "price"
                | "quality"
                | "spread"
                | "adjusted"
        ) {
            return false;
        }
    }
    true
}

fn normalize_runtime_formula_suffix(suffix: &str) -> String {
    suffix
        .replace(
            "_runtime_pass_through_add_spread_penalty",
            "_spread_adjusted",
        )
        .replace(
            "_runtime_pass_through_add_capacity_gate",
            "_full_depth_entry_gate",
        )
        .replace("_add_capacity_gate", "_full_depth_entry_gate")
}

fn strip_runtime_selector_gates(suffix: &str) -> Option<String> {
    let mut remaining_suffix = suffix.to_string();
    while let Some((remaining, selector)) = remaining_suffix.split_once("_select_") {
        let (_feature, raw_threshold, trailing_suffix) = parse_runtime_selector_gate(selector)?;
        parse_runtime_selector_threshold(raw_threshold)?;
        remaining_suffix = format!("{remaining}{trailing_suffix}");
    }
    Some(remaining_suffix)
}

fn parse_runtime_selector_gate(selector: &str) -> Option<(&'static str, &str, String)> {
    for feature in [
        "entry_price_quality",
        "full_depth_entry",
        "entry_capacity",
        "near_strike",
    ] {
        let prefix = format!("{feature}_ge_");
        let Some(raw) = selector.strip_prefix(&prefix) else {
            continue;
        };
        let (threshold, trailing_suffix) = match raw.split_once('_') {
            Some((threshold, trailing)) => (threshold, format!("_{trailing}")),
            None => (raw, String::new()),
        };
        return Some((feature, threshold, trailing_suffix));
    }
    None
}

fn parse_runtime_selector_threshold(raw: &str) -> Option<f64> {
    let threshold = if raw.contains('.') {
        raw.parse().ok()?
    } else {
        raw.parse::<f64>().ok()? / 100.0
    };
    (threshold.is_finite() && (0.0..=1.0).contains(&threshold)).then_some(threshold)
}

fn is_settlement_formula(normalized: &str) -> bool {
    [
        "auto_settlement_full_depth_settlement_edge",
        "auto_settlement_conservative_settlement_edge",
        "auto_settlement_model_full_depth_settlement_edge",
        "auto_settlement_model_conservative_settlement_edge",
    ]
    .iter()
    .any(|base| {
        normalized
            .strip_prefix(base)
            .map(settlement_formula_suffix_supported)
            .unwrap_or(false)
    })
}

fn settlement_formula_suffix_supported(suffix: &str) -> bool {
    let suffix = normalize_runtime_formula_suffix(suffix);
    let Some(suffix) = strip_runtime_selector_gates(&suffix) else {
        return false;
    };
    if suffix.is_empty() {
        return true;
    }
    let mut applied = BTreeSet::new();
    for token in suffix.trim_start_matches('_').split('_') {
        let effect = match token {
            "strike" => Some("near_strike"),
            "capacity" => Some("capacity"),
            "quality" => Some("entry_price_quality"),
            "adjusted" => Some("spread_adjusted"),
            "pressure" => Some("external_pressure"),
            "change" => Some("iv_change"),
            "gate" => Some("full_depth_entry_gate"),
            "squashed" => Some("squashed"),
            _ => None,
        };
        if let Some(effect) = effect {
            if !applied.insert(effect) {
                return false;
            }
            continue;
        }
        if !matches!(
            token,
            "x" | "near" | "full" | "depth" | "entry" | "price" | "spread" | "external" | "iv"
        ) {
            return false;
        }
    }
    true
}

fn factor_input_names(expr: &FactorExpr) -> Vec<String> {
    let mut names = BTreeSet::new();
    collect_factor_input_names(expr, &mut names);
    names.into_iter().collect()
}

fn collect_factor_input_names(expr: &FactorExpr, names: &mut BTreeSet<String>) {
    match expr {
        FactorExpr::Input(name) => {
            names.insert(name.clone());
        }
        FactorExpr::Const(_) => {}
        FactorExpr::Add(lhs, rhs)
        | FactorExpr::Sub(lhs, rhs)
        | FactorExpr::Mul(lhs, rhs)
        | FactorExpr::SafeDiv(lhs, rhs)
        | FactorExpr::Max(lhs, rhs)
        | FactorExpr::Min(lhs, rhs) => {
            collect_factor_input_names(lhs, names);
            collect_factor_input_names(rhs, names);
        }
        FactorExpr::Tanh(expr)
        | FactorExpr::Log1pAbs(expr)
        | FactorExpr::SqrtAbs(expr)
        | FactorExpr::Clip { expr, .. }
        | FactorExpr::Delta { expr, .. }
        | FactorExpr::RollingMean { expr, .. }
        | FactorExpr::RollingStd { expr, .. }
        | FactorExpr::ZScore { expr, .. } => collect_factor_input_names(expr, names),
        FactorExpr::Gate { expr, gate, .. } => {
            collect_factor_input_names(expr, names);
            collect_factor_input_names(gate, names);
        }
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), AlphaSearchArtifactError> {
    let raw = serde_json::to_string_pretty(value)?;
    std::fs::write(path, raw)?;
    Ok(())
}

fn default_hypotheses(target: &str) -> Vec<PriorHypothesis> {
    if target == "full_depth_settlement_executable_pnl" {
        vec![
            PriorHypothesis {
                id: "settlement_edge_after_execution_cost",
                hypothesis: "Settlement probability edge is valuable only after full-depth executable entry cost and PM fee are deducted.",
                expected_mechanism: "True q minus sweep price should rank event-side decisions when depth and quote freshness are adequate.",
                required_surfaces: vec![
                    "polymarket_full_clob_depth",
                    "official_settlement",
                    "probability_state",
                ],
            },
            PriorHypothesis {
                id: "capacity_and_near_strike_gate",
                hypothesis: "Settlement edge should be gated by near-strike state and executable capacity.",
                expected_mechanism: "Near-strike contracts are more sensitive to small external moves, but only deployable when the book can absorb the stake.",
                required_surfaces: vec!["event_geometry", "polymarket_full_clob_depth"],
            },
        ]
    } else {
        vec![PriorHypothesis {
            id: "repricing_after_pm_lag",
            hypothesis: "External market movement is more valuable when Polymarket quotes are stale or spread-adjusted friction is low.",
            expected_mechanism: "CEX movement can predict short-horizon PM quote repricing before the book updates.",
            required_surfaces: vec!["binance_price", "binance_l2", "polymarket_quote_ticks"],
        }]
    }
}

fn candidate_source(name: &str) -> &'static str {
    if name.starts_with("mut_") {
        "deterministic_mutation"
    } else if name.starts_with("auto_settlement_") {
        "settlement_native_generator"
    } else {
        "domain_seed"
    }
}

fn node_metric(
    idx: usize,
    report: &AutoFactorReport,
    runtime_avoidances: &[RuntimeAvoidance],
    alpha_zoo: Option<&AlphaZooSnapshot>,
    subtree_frequencies: &[SubtreeFrequencyState],
) -> NodeMetric {
    let matching_avoidance = matching_runtime_avoidance(report, runtime_avoidances);
    let simplicity = 1.0 / report.complexity.max(1) as f64;
    let structural_novelty = structural_novelty_score(&report.expr, subtree_frequencies);
    let diversity_penalty = structural_diversity_penalty(&report.expr, subtree_frequencies);
    NodeMetric {
        id: format!("node-{idx}"),
        factor_name: report.name.clone(),
        parent_name: report.parent_name.clone(),
        target: report.target.clone(),
        side: report.side,
        decision: report.decision.as_str().to_string(),
        reason: report.reason.clone(),
        selected_dimension: selected_dimension(report, runtime_avoidances),
        effectiveness: normalized_positive(report.top_bucket_avg_label),
        stability: report.positive_window_ratio.clamp(0.0, 1.0),
        diversity: structural_novelty,
        alpha_zoo_novelty: alpha_zoo_novelty_score(
            &report.expr,
            report.target.as_deref(),
            alpha_zoo,
        ),
        simplicity,
        structural_novelty,
        diversity_penalty,
        execution_cost: execution_score(report),
        event_uniqueness: event_uniqueness_score(report),
        overfit_risk: simplicity,
        runtime_readiness: if report.name.starts_with("auto_settlement_")
            || report.name == "amplitude_weighted_momentum_30s_sigma"
        {
            1.0
        } else {
            0.5
        },
        reward: reward(report, runtime_avoidances, alpha_zoo, subtree_frequencies),
        spearman_ic: finite_or_zero(report.spearman_ic),
        icir: finite_or_zero(report.icir),
        positive_window_ratio: finite_or_zero(report.positive_window_ratio),
        top_bucket_avg_label: finite_or_zero(report.top_bucket_avg_label),
        top_bucket_full_depth_entry_fill_rate: finite_or_zero(
            report.top_bucket_full_depth_entry_fill_rate,
        ),
        top_bucket_avg_entry_sweep_slippage_bps: finite_or_zero(
            report.top_bucket_avg_entry_sweep_slippage_bps,
        ),
        top_bucket_avg_entry_sweep_levels: finite_or_zero(report.top_bucket_avg_entry_sweep_levels),
        top_bucket_unique_event_count: report.top_bucket_unique_event_count,
        top_bucket_max_event_decisions: report.top_bucket_max_event_decisions,
        runtime_entry_signal_rate: matching_avoidance
            .and_then(|avoidance| avoidance.entry_signal_rate),
        runtime_executable_edge_pass_rate: matching_avoidance
            .and_then(|avoidance| avoidance.executable_edge_pass_rate),
        runtime_pass_through_penalty: runtime_pass_through_penalty(report, runtime_avoidances),
        alpha_zoo_penalty: alpha_zoo_novelty_penalty(
            &report.expr,
            report.target.as_deref(),
            alpha_zoo,
        ),
        monotonicity_score: finite_or_zero(report.monotonicity_score),
        complexity: report.complexity,
    }
}

fn formula_mcts_checkpoint(
    _artifact_version: &str,
    target: &str,
    side: Option<ReviewSide>,
    metrics: &[NodeMetric],
    prior_state: Option<&FormulaMctsCheckpoint>,
    subtree_frequencies: Vec<SubtreeFrequencyState>,
) -> Result<FormulaMctsCheckpoint, AlphaSearchArtifactError> {
    if let Some(prior) = prior_state {
        validate_formula_mcts_checkpoint(prior)?;
        if prior.target != target || prior.side != side {
            return Err(AlphaSearchArtifactError::IdentityMismatch(format!(
                "Formula MCTS checkpoint expected target={target} side={}, found target={} side={}",
                side.map(ReviewSide::as_str).unwrap_or("<none>"),
                prior.target,
                prior.side.map(ReviewSide::as_str).unwrap_or("<none>")
            )));
        }
    }

    let mut records = prior_state
        .map(|state| {
            state
                .nodes
                .iter()
                .map(|node| (node.factor_name.clone(), node.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    records
        .entry(FORMULA_MCTS_ROOT_ID.to_string())
        .or_insert_with(|| FormulaMctsCheckpointNode {
            factor_name: FORMULA_MCTS_ROOT_ID.to_string(),
            parent_name: None,
            visits: 0,
            total_reward: 0.0,
            best_reward: None,
            last_reward: 0.0,
            selected_dimension: "root".to_string(),
            last_decision: "root".to_string(),
        });

    for metric in metrics {
        if metric.factor_name == FORMULA_MCTS_ROOT_ID {
            return Err(AlphaSearchArtifactError::IdentityMismatch(
                "Formula candidate identity uses the reserved MCTS root id".to_string(),
            ));
        }
        let parent_name = metric.parent_name.clone();
        match records.get(&metric.factor_name) {
            Some(node) if node.parent_name != parent_name => {
                return Err(AlphaSearchArtifactError::IdentityMismatch(format!(
                    "Formula candidate `{}` changed parent from {:?} to {:?}",
                    metric.factor_name, node.parent_name, parent_name
                )));
            }
            Some(_) => {}
            None => {
                records.insert(
                    metric.factor_name.clone(),
                    FormulaMctsCheckpointNode {
                        factor_name: metric.factor_name.clone(),
                        parent_name,
                        visits: 0,
                        total_reward: 0.0,
                        best_reward: None,
                        last_reward: 0.0,
                        selected_dimension: metric.selected_dimension.clone(),
                        last_decision: metric.decision.clone(),
                    },
                );
            }
        }
    }

    let mut nodes = formula_mcts_kernel_nodes(records)?;
    for metric in metrics {
        let node_id = nodes
            .iter()
            .position(|node| node.record.factor_name == metric.factor_name)
            .ok_or_else(|| {
                AlphaSearchArtifactError::IdentityMismatch(format!(
                    "Formula candidate `{}` disappeared from its MCTS checkpoint",
                    metric.factor_name
                ))
            })?;
        nodes[node_id].record.last_reward = metric.reward;
        nodes[node_id].record.selected_dimension = metric.selected_dimension.clone();
        nodes[node_id].record.last_decision = metric.decision.clone();
        kernel_backpropagate(&mut nodes, 0, node_id, metric.reward)?;
    }

    validate_kernel_tree(&nodes, 0, usize::MAX)?;
    Ok(FormulaMctsCheckpoint {
        version: FORMULA_MCTS_CHECKPOINT_VERSION.to_string(),
        target: target.to_string(),
        side,
        selection_budget: FORMULA_MCTS_SELECTION_BUDGET,
        nodes: nodes.into_iter().map(|node| node.record).collect(),
        subtree_frequencies,
    })
}

fn mcts_search_state_projection(
    checkpoint: &FormulaMctsCheckpoint,
) -> Result<MctsSearchStateArtifact, AlphaSearchArtifactError> {
    validate_formula_mcts_checkpoint(checkpoint)?;
    let root = checkpoint
        .nodes
        .first()
        .expect("validated Formula MCTS checkpoint has a root");
    let mut nodes = checkpoint
        .nodes
        .iter()
        .filter(|node| node.factor_name != FORMULA_MCTS_ROOT_ID)
        .map(|node| {
            Ok(MctsSearchStateNode {
                factor_name: node.factor_name.clone(),
                parent_name: node.parent_name.clone(),
                visits: usize::try_from(node.visits).map_err(|_| UctError::StatsOverflow)?,
                total_reward: node.total_reward,
                best_reward: node.best_reward.unwrap_or(0.0),
                last_reward: node.last_reward,
                selected_dimension: node.selected_dimension.clone(),
                last_decision: node.last_decision.clone(),
            })
        })
        .collect::<Result<Vec<_>, AlphaSearchArtifactError>>()?;
    nodes.sort_by(|lhs, rhs| lhs.factor_name.cmp(&rhs.factor_name));
    Ok(MctsSearchStateArtifact {
        version: FORMULA_MCTS_CHECKPOINT_VERSION.to_string(),
        mode: "shared_kernel_projection".to_string(),
        target: checkpoint.target.clone(),
        side: checkpoint.side,
        total_visits: usize::try_from(root.visits).map_err(|_| UctError::StatsOverflow)?,
        backpropagation_truncated_count: 0,
        nodes,
        subtree_frequencies: checkpoint.subtree_frequencies.clone(),
    })
}

#[cfg(test)]
fn mcts_search_state(
    version: &str,
    target: &str,
    side: Option<ReviewSide>,
    metrics: &[NodeMetric],
    prior_state: Option<&FormulaMctsCheckpoint>,
    subtree_frequencies: Vec<SubtreeFrequencyState>,
) -> MctsSearchStateArtifact {
    let checkpoint = formula_mcts_checkpoint(
        version,
        target,
        side,
        metrics,
        prior_state,
        subtree_frequencies,
    )
    .expect("test Formula MCTS checkpoint");
    mcts_search_state_projection(&checkpoint).expect("test Formula MCTS projection")
}

fn formula_mcts_expansion_plan(
    version: &'static str,
    target: &str,
    side: Option<ReviewSide>,
    checkpoint: &FormulaMctsCheckpoint,
) -> Result<MctsExpansionPlan, AlphaSearchArtifactError> {
    if checkpoint.target != target || checkpoint.side != side {
        return Err(AlphaSearchArtifactError::IdentityMismatch(
            "Formula MCTS checkpoint identity does not match expansion plan".to_string(),
        ));
    }
    let exploration_weight = 0.75;
    let mut nodes = formula_mcts_kernel_nodes(
        checkpoint
            .nodes
            .iter()
            .map(|node| (node.factor_name.clone(), node.clone()))
            .collect(),
    )?;
    let mut selected = Vec::new();
    while selected.len() < checkpoint.selection_budget {
        let Some(node_id) = kernel_select_expandable(&nodes, 0, exploration_weight)? else {
            break;
        };
        if node_id == 0 {
            break;
        }
        let node = &mut nodes[node_id];
        node.expandable = false;
        selected.push(MctsSelectedNode {
            node_id: node.record.factor_name.clone(),
            factor_name: node.record.factor_name.clone(),
            selected_dimension: node.record.selected_dimension.clone(),
            proposed_mutation: proposed_mutation(&node.record.selected_dimension),
            reward: node.record.last_reward,
        });
    }

    Ok(MctsExpansionPlan {
        version,
        mode: "shared_kernel_selection_projection",
        target: target.to_string(),
        side,
        exploration_weight,
        selected_nodes: selected,
        note: "Formula candidate identity is adapter metadata around the shared UCT kernel; this plan is a read-only projection of kernel selection.",
    })
}

#[derive(Clone)]
struct FormulaMctsKernelNode {
    record: FormulaMctsCheckpointNode,
    parent: Option<usize>,
    children: Vec<usize>,
    depth: usize,
    expandable: bool,
}

impl UctNode for FormulaMctsKernelNode {
    fn parent(&self) -> Option<usize> {
        self.parent
    }

    fn children(&self) -> &[usize] {
        &self.children
    }

    fn is_expandable(&self) -> bool {
        self.expandable
    }

    fn depth(&self) -> usize {
        self.depth
    }

    fn stats(&self) -> Result<UctStats, UctError> {
        UctStats::from_parts(
            self.record.visits,
            self.record.total_reward,
            self.record.best_reward,
        )
    }

    fn replace_stats(&mut self, stats: UctStats) {
        self.record.visits = stats.visits();
        self.record.total_reward = stats.total_reward();
        self.record.best_reward = stats.best_reward();
    }
}

fn formula_mcts_kernel_nodes(
    mut records: BTreeMap<String, FormulaMctsCheckpointNode>,
) -> Result<Vec<FormulaMctsKernelNode>, AlphaSearchArtifactError> {
    let root = records.remove(FORMULA_MCTS_ROOT_ID).ok_or_else(|| {
        AlphaSearchArtifactError::IdentityMismatch(
            "Formula MCTS checkpoint is missing its reserved root node".to_string(),
        )
    })?;
    if root.parent_name.is_some() || root.factor_name != FORMULA_MCTS_ROOT_ID {
        return Err(AlphaSearchArtifactError::IdentityMismatch(
            "Formula MCTS checkpoint root identity is invalid".to_string(),
        ));
    }
    let mut nodes = vec![FormulaMctsKernelNode {
        record: root,
        parent: None,
        children: Vec::new(),
        depth: 0,
        expandable: false,
    }];
    let mut by_name = BTreeMap::from([(FORMULA_MCTS_ROOT_ID.to_string(), 0usize)]);

    while !records.is_empty() {
        let ready = records
            .iter()
            .filter_map(|(name, node)| {
                let parent = node.parent_name.as_deref().unwrap_or(FORMULA_MCTS_ROOT_ID);
                by_name.contains_key(parent).then_some(name.clone())
            })
            .collect::<Vec<_>>();
        if ready.is_empty() {
            let unresolved = records.keys().cloned().collect::<Vec<_>>().join(", ");
            return Err(AlphaSearchArtifactError::IdentityMismatch(format!(
                "Formula MCTS candidate lineage is cyclic or references a missing parent: {unresolved}"
            )));
        }
        for name in ready {
            let record = records.remove(&name).expect("ready Formula node exists");
            if record.factor_name != name {
                return Err(AlphaSearchArtifactError::IdentityMismatch(format!(
                    "Formula MCTS checkpoint key `{name}` does not match candidate identity `{}`",
                    record.factor_name
                )));
            }
            let parent_name = record
                .parent_name
                .as_deref()
                .unwrap_or(FORMULA_MCTS_ROOT_ID);
            let parent = *by_name.get(parent_name).expect("ready parent is assigned");
            let depth = nodes[parent].depth.saturating_add(1);
            let node_id = nodes.len();
            nodes[parent].children.push(node_id);
            nodes.push(FormulaMctsKernelNode {
                expandable: false,
                record,
                parent: Some(parent),
                children: Vec::new(),
                depth,
            });
            by_name.insert(name, node_id);
        }
    }
    for node in &mut nodes {
        node.expandable = node.parent.is_some()
            && node.children.is_empty()
            && node.record.last_decision != "reject"
            && node.record.selected_dimension != "runtime_executable_entry";
    }
    validate_kernel_tree(&nodes, 0, usize::MAX)?;
    Ok(nodes)
}

fn validate_formula_mcts_checkpoint(
    checkpoint: &FormulaMctsCheckpoint,
) -> Result<(), AlphaSearchArtifactError> {
    if checkpoint.version != FORMULA_MCTS_CHECKPOINT_VERSION {
        return Err(AlphaSearchArtifactError::LegacyCheckpointVersion(
            checkpoint.version.clone(),
        ));
    }
    if checkpoint.selection_budget != FORMULA_MCTS_SELECTION_BUDGET {
        return Err(AlphaSearchArtifactError::IdentityMismatch(format!(
            "Formula MCTS checkpoint selection budget {} does not match {FORMULA_MCTS_SELECTION_BUDGET}",
            checkpoint.selection_budget
        )));
    }
    if checkpoint
        .nodes
        .first()
        .map(|node| node.factor_name.as_str())
        != Some(FORMULA_MCTS_ROOT_ID)
    {
        return Err(AlphaSearchArtifactError::IdentityMismatch(
            "Formula MCTS checkpoint root must be the first node".to_string(),
        ));
    }
    let records = checkpoint
        .nodes
        .iter()
        .map(|node| (node.factor_name.clone(), node.clone()))
        .collect::<BTreeMap<_, _>>();
    if records.len() != checkpoint.nodes.len() {
        return Err(AlphaSearchArtifactError::IdentityMismatch(
            "Formula MCTS checkpoint contains duplicate candidate identities".to_string(),
        ));
    }
    formula_mcts_kernel_nodes(records)?;
    Ok(())
}

#[cfg(test)]
fn mcts_expansion_plan(
    version: &'static str,
    target: &str,
    side: Option<ReviewSide>,
    metrics: &[NodeMetric],
    _state: &MctsSearchStateArtifact,
) -> MctsExpansionPlan {
    let checkpoint = formula_mcts_checkpoint(version, target, side, metrics, None, Vec::new())
        .expect("test Formula MCTS checkpoint");
    formula_mcts_expansion_plan(version, target, side, &checkpoint)
        .expect("test Formula MCTS expansion plan")
}

fn proposed_mutation(selected_dimension: &str) -> &'static str {
    match selected_dimension {
        "runtime_executable_entry" => "add_spread_penalty",
        "sample_power" => "do_not_expand_collect_more_data",
        "stability" => "add_feature_gate",
        "effectiveness" => "replace_denominator",
        "monotonicity" => "clip_or_squash",
        "execution_quality" => "add_capacity_gate",
        "event_uniqueness" => "add_capacity_gate",
        "overfit_risk" => "remove_component",
        "exploit" => "add_capacity_gate",
        _ => "clip_or_squash",
    }
}

fn reward(
    report: &AutoFactorReport,
    runtime_avoidances: &[RuntimeAvoidance],
    alpha_zoo: Option<&AlphaZooSnapshot>,
    subtree_frequencies: &[SubtreeFrequencyState],
) -> f64 {
    let decision_bonus = match report.decision {
        AutoFactorDecision::Candidate => 1.0,
        AutoFactorDecision::Watchlist => 0.25,
        AutoFactorDecision::Reject => -0.5,
    };
    decision_bonus
        + finite_or_zero(report.icir).tanh()
        + finite_or_zero(report.spearman_ic).tanh()
        + finite_or_zero(report.positive_window_ratio)
        + normalized_positive(report.top_bucket_avg_label)
        + execution_score(report)
        + event_uniqueness_score(report)
        + finite_or_zero(report.monotonicity_score)
        - event_decision_penalty(report)
        - execution_penalty(report)
        - runtime_pass_through_penalty(report, runtime_avoidances)
        - alpha_zoo_novelty_penalty(&report.expr, report.target.as_deref(), alpha_zoo)
        - structural_diversity_penalty(&report.expr, subtree_frequencies)
        - (report.complexity as f64 / 32.0)
}

fn execution_score(report: &AutoFactorReport) -> f64 {
    let top_bucket_fillability = finite_or_zero(report.top_bucket_full_depth_entry_fill_rate);
    let slippage_bps = finite_or_zero(report.top_bucket_avg_entry_sweep_slippage_bps);
    let levels = finite_or_zero(report.top_bucket_avg_entry_sweep_levels);
    let slippage_score = if slippage_bps <= 0.0 {
        1.0
    } else {
        (1.0 - (slippage_bps / 200.0)).clamp(0.0, 1.0)
    };
    let levels_score = if levels <= 0.0 {
        1.0
    } else {
        (1.0 - ((levels - 1.0) / 2.0)).clamp(0.0, 1.0)
    };
    let structure_bonus = if report.name.contains("capacity") || report.name.contains("spread") {
        0.25
    } else {
        0.0
    };
    (top_bucket_fillability * slippage_score * levels_score + structure_bonus).clamp(0.0, 1.0)
}

fn execution_penalty(report: &AutoFactorReport) -> f64 {
    let fillability = finite_or_zero(report.top_bucket_full_depth_entry_fill_rate);
    let slippage_bps = finite_or_zero(report.top_bucket_avg_entry_sweep_slippage_bps);
    let levels = finite_or_zero(report.top_bucket_avg_entry_sweep_levels);
    let fillability_penalty = if report.top_bucket_n > 0 && fillability < 0.30 {
        (0.30 - fillability) * 2.0
    } else {
        0.0
    };
    let slippage_penalty = if slippage_bps > 200.0 {
        ((slippage_bps - 200.0) / 200.0).min(4.0)
    } else {
        0.0
    };
    let levels_penalty = if levels > 3.0 {
        ((levels - 3.0) * 0.5).min(2.0)
    } else {
        0.0
    };
    fillability_penalty + slippage_penalty + levels_penalty
}

fn event_uniqueness_score(report: &AutoFactorReport) -> f64 {
    if report.top_bucket_n == 0 {
        return 0.0;
    }
    let unique_ratio =
        (report.top_bucket_unique_event_count as f64 / report.top_bucket_n as f64).clamp(0.0, 1.0);
    let decision_ratio = if report.top_bucket_max_event_decisions <= 1 {
        1.0
    } else {
        1.0 / report.top_bucket_max_event_decisions as f64
    };
    unique_ratio * decision_ratio
}

fn event_decision_penalty(report: &AutoFactorReport) -> f64 {
    if report.top_bucket_n > 0 && report.top_bucket_unique_event_count == 0 {
        return 1.0;
    }
    if report.top_bucket_max_event_decisions <= 1 {
        0.0
    } else {
        ((report.top_bucket_max_event_decisions - 1) as f64 * 1.5).min(6.0)
    }
}

fn selected_dimension(
    report: &AutoFactorReport,
    runtime_avoidances: &[RuntimeAvoidance],
) -> String {
    if runtime_pass_through_penalty(report, runtime_avoidances) >= 8.0 {
        return "runtime_executable_entry".to_string();
    }
    if report.top_bucket_n > 0
        && (report.top_bucket_unique_event_count == 0 || report.top_bucket_max_event_decisions > 1)
    {
        return "event_uniqueness".to_string();
    }
    if report.top_bucket_n > 0
        && (report.top_bucket_full_depth_entry_fill_rate < 0.30
            || report.top_bucket_avg_entry_sweep_slippage_bps > 200.0
            || report.top_bucket_avg_entry_sweep_levels > 3.0)
    {
        return "execution_quality".to_string();
    }
    match report.reason.as_str() {
        "too_few_observations" | "no_powered_windows" => "sample_power",
        "low_icir" | "unstable_positive_windows" => "stability",
        "nonpositive_top_bucket_label" | "nonpositive_rank_ic" => "effectiveness",
        "low_top_bucket_fillability" => "execution_quality",
        "nonmonotonic_buckets" => "monotonicity",
        "too_complex" => "overfit_risk",
        "passed" => "exploit",
        _ => "unknown",
    }
    .to_string()
}

fn subtree_frequency_state(
    target: &str,
    reports: &[AutoFactorReport],
    prior_state: Option<&FormulaMctsCheckpoint>,
    llm_prior: Option<&LlmPriorSpec>,
) -> Vec<SubtreeFrequencyState> {
    let mut counts: BTreeMap<String, StructuralSubtreeCount> = prior_state
        .filter(|state| state.target == target)
        .map(|state| {
            state
                .subtree_frequencies
                .iter()
                .map(|item| {
                    (
                        item.structural_signature.clone(),
                        StructuralSubtreeCount {
                            root_gene: item.root_gene.clone(),
                            depth: item.depth,
                            count: item.count,
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    for report in reports {
        record_subtree_count(&mut counts, &report.expr);
        for subtree in inner_structural_subtrees(&report.expr) {
            record_subtree_count(&mut counts, subtree);
        }
    }

    if let Some(prior) = llm_prior {
        for item in &prior.structural_avoid_signatures {
            let signature = item.structural_signature.trim();
            if signature.is_empty() {
                continue;
            }
            let entry =
                counts
                    .entry(signature.to_string())
                    .or_insert_with(|| StructuralSubtreeCount {
                        root_gene: item.root_gene.clone().unwrap_or_default(),
                        depth: 0,
                        count: 0,
                    });
            if entry.root_gene.is_empty() {
                entry.root_gene = item.root_gene.clone().unwrap_or_default();
            }
            entry.count = entry.count.max(item.count.max(3));
        }
    }

    counts
        .into_iter()
        .map(|(structural_signature, item)| SubtreeFrequencyState {
            root_gene: item.root_gene,
            structural_signature,
            depth: item.depth,
            count: item.count,
        })
        .collect()
}

fn avoided_subtrees(
    side: Option<ReviewSide>,
    frequencies: &[SubtreeFrequencyState],
) -> Vec<AvoidedSubtree> {
    frequencies
        .iter()
        .filter(|item| item.count > 2)
        .map(|item| AvoidedSubtree {
            side,
            root_gene: item.root_gene.clone(),
            structural_signature: item.structural_signature.clone(),
            depth: item.depth,
            count: item.count,
            action: "penalize",
            reason: "structural_signature_crowding",
        })
        .collect()
}

#[derive(Debug, Default)]
struct StructuralSubtreeCount {
    root_gene: String,
    depth: usize,
    count: usize,
}

fn record_subtree_count(counts: &mut BTreeMap<String, StructuralSubtreeCount>, expr: &FactorExpr) {
    let signature = structural_signature(expr);
    let entry = counts
        .entry(signature)
        .or_insert_with(|| StructuralSubtreeCount {
            root_gene: root_gene(expr),
            depth: structural_depth(expr),
            count: 0,
        });
    entry.count = entry.count.saturating_add(1);
}

fn structural_diversity_penalty(expr: &FactorExpr, frequencies: &[SubtreeFrequencyState]) -> f64 {
    let max_count = max_structural_frequency(expr, frequencies);
    if max_count <= 2 {
        0.0
    } else {
        ((max_count - 2) as f64 * 0.35).min(2.0)
    }
}

fn structural_novelty_score(expr: &FactorExpr, frequencies: &[SubtreeFrequencyState]) -> f64 {
    1.0 / max_structural_frequency(expr, frequencies).max(1) as f64
}

fn max_structural_frequency(expr: &FactorExpr, frequencies: &[SubtreeFrequencyState]) -> usize {
    let signatures = structural_signatures(expr);
    frequencies
        .iter()
        .filter(|item| signatures.contains(&item.structural_signature))
        .map(|item| item.count)
        .max()
        .unwrap_or(1)
}

fn structural_signatures(expr: &FactorExpr) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    out.insert(structural_signature(expr));
    for subtree in inner_structural_subtrees(expr) {
        out.insert(structural_signature(subtree));
    }
    out
}

fn structural_signature(expr: &FactorExpr) -> String {
    match expr {
        FactorExpr::Input(name) => format!("Input({name})"),
        FactorExpr::Const(_) => "Const(_)".to_string(),
        FactorExpr::Add(lhs, rhs) => commutative_signature("Add", lhs, rhs),
        FactorExpr::Sub(lhs, rhs) => {
            format!(
                "Sub({},{})",
                structural_signature(lhs),
                structural_signature(rhs)
            )
        }
        FactorExpr::Mul(lhs, rhs) => commutative_signature("Mul", lhs, rhs),
        FactorExpr::SafeDiv(lhs, rhs) => {
            format!(
                "SafeDiv({},{})",
                structural_signature(lhs),
                structural_signature(rhs)
            )
        }
        FactorExpr::Max(lhs, rhs) => commutative_signature("Max", lhs, rhs),
        FactorExpr::Min(lhs, rhs) => commutative_signature("Min", lhs, rhs),
        FactorExpr::Tanh(inner) => format!("Tanh({})", structural_signature(inner)),
        FactorExpr::Log1pAbs(inner) => format!("Log1pAbs({})", structural_signature(inner)),
        FactorExpr::SqrtAbs(inner) => format!("SqrtAbs({})", structural_signature(inner)),
        FactorExpr::Clip { expr, .. } => format!("Clip(_,_,{})", structural_signature(expr)),
        FactorExpr::Delta { expr, .. } => format!("Delta(_,{})", structural_signature(expr)),
        FactorExpr::RollingMean { expr, .. } => {
            format!("RollingMean(_,{})", structural_signature(expr))
        }
        FactorExpr::RollingStd { expr, .. } => {
            format!("RollingStd(_,{})", structural_signature(expr))
        }
        FactorExpr::ZScore { expr, .. } => format!("ZScore(_,{})", structural_signature(expr)),
        FactorExpr::Gate { expr, gate, .. } => {
            format!(
                "Gate(_,{},{})",
                structural_signature(expr),
                structural_signature(gate)
            )
        }
    }
}

fn commutative_signature(op: &str, lhs: &FactorExpr, rhs: &FactorExpr) -> String {
    let mut children = [structural_signature(lhs), structural_signature(rhs)];
    children.sort();
    format!("{op}({},{})", children[0], children[1])
}

fn inner_structural_subtrees(expr: &FactorExpr) -> Vec<&FactorExpr> {
    let mut out = Vec::new();
    collect_inner_structural_subtrees(expr, false, &mut out);
    out
}

fn collect_inner_structural_subtrees<'a>(
    expr: &'a FactorExpr,
    include_current: bool,
    out: &mut Vec<&'a FactorExpr>,
) {
    if include_current && structural_depth(expr) >= 2 {
        out.push(expr);
    }
    match expr {
        FactorExpr::Input(_) | FactorExpr::Const(_) => {}
        FactorExpr::Add(lhs, rhs)
        | FactorExpr::Sub(lhs, rhs)
        | FactorExpr::Mul(lhs, rhs)
        | FactorExpr::SafeDiv(lhs, rhs)
        | FactorExpr::Max(lhs, rhs)
        | FactorExpr::Min(lhs, rhs) => {
            collect_inner_structural_subtrees(lhs, true, out);
            collect_inner_structural_subtrees(rhs, true, out);
        }
        FactorExpr::Tanh(inner)
        | FactorExpr::Log1pAbs(inner)
        | FactorExpr::SqrtAbs(inner)
        | FactorExpr::Clip { expr: inner, .. }
        | FactorExpr::Delta { expr: inner, .. }
        | FactorExpr::RollingMean { expr: inner, .. }
        | FactorExpr::RollingStd { expr: inner, .. }
        | FactorExpr::ZScore { expr: inner, .. } => {
            collect_inner_structural_subtrees(inner, true, out);
        }
        FactorExpr::Gate { expr, gate, .. } => {
            collect_inner_structural_subtrees(expr, true, out);
            collect_inner_structural_subtrees(gate, true, out);
        }
    }
}

fn structural_depth(expr: &FactorExpr) -> usize {
    match expr {
        FactorExpr::Input(_) | FactorExpr::Const(_) => 1,
        FactorExpr::Add(lhs, rhs)
        | FactorExpr::Sub(lhs, rhs)
        | FactorExpr::Mul(lhs, rhs)
        | FactorExpr::SafeDiv(lhs, rhs)
        | FactorExpr::Max(lhs, rhs)
        | FactorExpr::Min(lhs, rhs) => 1 + structural_depth(lhs).max(structural_depth(rhs)),
        FactorExpr::Tanh(inner)
        | FactorExpr::Log1pAbs(inner)
        | FactorExpr::SqrtAbs(inner)
        | FactorExpr::Clip { expr: inner, .. }
        | FactorExpr::Delta { expr: inner, .. }
        | FactorExpr::RollingMean { expr: inner, .. }
        | FactorExpr::RollingStd { expr: inner, .. }
        | FactorExpr::ZScore { expr: inner, .. } => 1 + structural_depth(inner),
        FactorExpr::Gate { expr, gate, .. } => {
            1 + structural_depth(expr).max(structural_depth(gate))
        }
    }
}

/// Coarse root-operator-only structural fingerprint, shared by both the
/// batch-local Frequent-Subtree Avoidance path (`avoided_subtrees`) and the
/// cross-run Alpha Zoo grouping in `examples/persist_research_trace.rs`, so
/// both diversity controls agree on what counts as "the same shape". Exported
/// so callers outside this module (e.g. the `factor_registry` export path)
/// can group historical rows the same way without duplicating this logic.
pub fn root_gene(expr: &FactorExpr) -> String {
    match expr {
        FactorExpr::Input(_) => "Input",
        FactorExpr::Const(_) => "Const",
        FactorExpr::Add(_, _) => "Add",
        FactorExpr::Sub(_, _) => "Sub",
        FactorExpr::Mul(_, _) => "Mul",
        FactorExpr::SafeDiv(_, _) => "SafeDiv",
        FactorExpr::Max(_, _) => "Max",
        FactorExpr::Min(_, _) => "Min",
        FactorExpr::Tanh(_) => "Tanh",
        FactorExpr::Log1pAbs(_) => "Log1pAbs",
        FactorExpr::SqrtAbs(_) => "SqrtAbs",
        FactorExpr::Clip { .. } => "Clip",
        FactorExpr::Delta { .. } => "Delta",
        FactorExpr::RollingMean { .. } => "RollingMean",
        FactorExpr::RollingStd { .. } => "RollingStd",
        FactorExpr::ZScore { .. } => "ZScore",
        FactorExpr::Gate { .. } => "Gate",
    }
    .to_string()
}

/// Look up how many historically-accepted factors (across all runs, sourced
/// from the durable `factor_registry` table) share this candidate's root
/// gene. Returns `None` when there is no Alpha Zoo snapshot, the snapshot was
/// exported for a different search target, or there is no matching entry.
///
/// The `target` check matters because reprice and settlement targets have
/// unrelated factor populations: a snapshot exported for
/// `full_depth_settlement_executable_pnl` must not penalize
/// `full_depth_reprice_pnl_10s` candidates just because they share a root
/// operator. Without this check, a caller that scores multiple targets with
/// the same snapshot (see `monday-prediction-evaluator`) would cross-contaminate
/// unrelated target populations.
///
/// NOTE: this reuses the coarse root-operator-only `root_gene()` fingerprint
/// on purpose. A finer-grained `structural_signature()` exists only in the
/// still-unmerged Frequent-Subtree Avoidance PR; this can be upgraded to that
/// signature once it lands on `main`.
fn alpha_zoo_matching_count(
    expr: &FactorExpr,
    target: &str,
    zoo: Option<&AlphaZooSnapshot>,
) -> Option<usize> {
    let zoo = zoo.filter(|zoo| zoo.target == target)?;
    let gene = root_gene(expr);
    zoo.entries
        .iter()
        .find(|entry| entry.root_gene == gene)
        .map(|entry| entry.count)
}

/// Alpha Zoo novelty penalty: `0.0` (no-op) when there is no zoo snapshot, the
/// snapshot targets a different search target, or the candidate has no
/// target, mirroring how `AlphaSearchRuntimeFeedback` and `LlmPriorSpec`
/// behave when absent. Otherwise, penalize candidates whose root gene is
/// over-represented across ALL historical accepted factors for this target.
///
/// The threshold is `5`, higher than the batch-local Frequent-Subtree
/// Avoidance threshold of `2` (see `avoided_subtrees` above), because the
/// Alpha Zoo aggregates every historical run rather than just the current
/// batch, so a much larger population of a single root gene is expected and
/// tolerable before it counts as crowding.
fn alpha_zoo_novelty_penalty(
    expr: &FactorExpr,
    target: Option<&str>,
    zoo: Option<&AlphaZooSnapshot>,
) -> f64 {
    const ALPHA_ZOO_CROWDING_THRESHOLD: usize = 5;
    match target.and_then(|target| alpha_zoo_matching_count(expr, target, zoo)) {
        Some(count) if count > ALPHA_ZOO_CROWDING_THRESHOLD => {
            ((count - ALPHA_ZOO_CROWDING_THRESHOLD) as f64 * 0.5).min(6.0)
        }
        _ => 0.0,
    }
}

/// Normalized Alpha Zoo novelty score in `(0.0, 1.0]`: `1.0` when there is no
/// zoo snapshot, the snapshot targets a different search target, the
/// candidate has no target, or there is no matching root gene (maximally
/// novel), otherwise `1.0 / count`, mirroring the shape of other normalized
/// score helpers in this file (e.g. `diversity`, `overfit_risk`).
fn alpha_zoo_novelty_score(
    expr: &FactorExpr,
    target: Option<&str>,
    zoo: Option<&AlphaZooSnapshot>,
) -> f64 {
    match target.and_then(|target| alpha_zoo_matching_count(expr, target, zoo)) {
        Some(count) => 1.0 / count.max(1) as f64,
        None => 1.0,
    }
}

fn normalized_positive(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value.tanh()
    } else {
        0.0
    }
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn ratio_usize(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn runtime_avoidances(
    runtime_feedback: Option<&AlphaSearchRuntimeFeedback>,
    llm_prior: Option<&LlmPriorSpec>,
) -> Vec<RuntimeAvoidance> {
    let mut out = Vec::new();
    if let Some(feedback) = runtime_feedback.filter(|feedback| feedback.is_pass_through_collapse())
    {
        out.push(RuntimeAvoidance {
            base_factor: feedback.base_factor.clone(),
            factor_family: normalized_factor_family(&feedback.base_factor),
            dsl_hash: feedback.dsl_hash.clone(),
            runtime_score: Some(feedback.runtime_score.clone()),
            reason: Some("runtime_pass_through_collapse".to_string()),
            source: "runtime_replay_feedback",
            entry_signal_rate: Some(feedback.entry_signal_rate()),
            executable_edge_pass_rate: Some(feedback.executable_edge_pass_rate()),
            penalty: runtime_feedback_penalty(feedback),
        });
    }
    if let Some(prior) = llm_prior {
        for item in &prior.runtime_avoid_factors {
            let family = item
                .factor_family
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(normalized_factor_family)
                .unwrap_or_else(|| normalized_factor_family(&item.base_factor));
            if family.is_empty() {
                continue;
            }
            let duplicate = out.iter().any(|existing| {
                existing.factor_family == family
                    || existing.base_factor == item.base_factor
                    || (existing.runtime_score.as_deref().is_some()
                        && existing.runtime_score.as_deref() == item.runtime_score.as_deref())
            });
            if duplicate {
                continue;
            }
            out.push(RuntimeAvoidance {
                base_factor: item.base_factor.clone(),
                factor_family: family,
                dsl_hash: None,
                runtime_score: item.runtime_score.clone(),
                reason: item.reason.clone(),
                source: "typed_prior",
                entry_signal_rate: None,
                executable_edge_pass_rate: None,
                penalty: 12.0,
            });
        }
    }
    out
}

fn matching_runtime_avoidance<'a>(
    report: &AutoFactorReport,
    runtime_avoidances: &'a [RuntimeAvoidance],
) -> Option<&'a RuntimeAvoidance> {
    let name = normalized_factor_key(&report.name);
    let family = normalized_factor_family(&report.name);
    let dsl_hash = runtime_avoidances
        .iter()
        .any(|avoidance| avoidance.dsl_hash.is_some())
        .then(|| factor_expr_hash(&report.expr).ok())
        .flatten();
    runtime_avoidances.iter().find(|avoidance| {
        avoidance
            .dsl_hash
            .as_deref()
            .is_none_or(|expected| dsl_hash.as_deref() == Some(expected))
            && !avoidance.factor_family.is_empty()
            && (family == avoidance.factor_family
                || name == avoidance.factor_family
                || name == normalized_factor_key(&avoidance.base_factor))
    })
}

fn runtime_pass_through_penalty(
    report: &AutoFactorReport,
    runtime_avoidances: &[RuntimeAvoidance],
) -> f64 {
    matching_runtime_avoidance(report, runtime_avoidances)
        .map(|avoidance| avoidance.penalty)
        .unwrap_or(0.0)
}

fn runtime_feedback_penalty(feedback: &AlphaSearchRuntimeFeedback) -> f64 {
    let entry_penalty = if feedback.direct_passes_at_configured_threshold >= 50 {
        let shortfall = 1.0 - feedback.entry_signal_rate();
        (shortfall * 5.0).clamp(0.0, 5.0)
    } else {
        0.0
    };
    let edge_penalty = if feedback.formula_evaluations >= 500 {
        let shortfall = 0.02 - feedback.executable_edge_pass_rate();
        if shortfall > 0.0 {
            (shortfall / 0.02 * 5.0).clamp(0.0, 5.0)
        } else {
            0.0
        }
    } else {
        0.0
    };
    (entry_penalty + edge_penalty + 4.0).min(12.0)
}

fn normalized_factor_family(raw: &str) -> String {
    let mut value = normalized_factor_key(raw);
    let suffixes = [
        "_select_entry_price_quality_ge_075",
        "_select_entry_price_quality_ge_050",
        "_select_entry_price_quality_ge_025",
        "_select_full_depth_entry_ge_075",
        "_select_full_depth_entry_ge_050",
        "_select_full_depth_entry_ge_025",
        "_select_near_strike_ge_075",
        "_select_near_strike_ge_050",
        "_select_near_strike_ge_025",
        "_runtime_pass_through_add_spread_penalty",
        "_runtime_pass_through_add_capacity_gate",
        "_add_spread_penalty",
        "_add_capacity_gate",
        "_add_feature_gate",
        "_entry_price_quality",
        "_full_depth_entry_gate",
        "_spread_adjusted",
        "_near_strike",
        "_capacity",
        "_squashed",
        "_pm_lag",
        "_clip",
    ];
    loop {
        let mut changed = false;
        for suffix in suffixes {
            if let Some(stripped) = value.strip_suffix(suffix) {
                if !stripped.is_empty() {
                    value = stripped.to_string();
                    changed = true;
                    break;
                }
            }
        }
        if !changed && value.ends_with("_x") && value.len() > 2 {
            value.truncate(value.len() - 2);
            changed = true;
        }
        if !changed {
            break;
        }
    }
    value
}

fn normalized_factor_key(raw: &str) -> String {
    let mut value = raw
        .strip_prefix("autofactor_formula:")
        .unwrap_or(raw)
        .to_string();
    loop {
        let next = value
            .strip_prefix("mut2_")
            .or_else(|| value.strip_prefix("llm_"))
            .or_else(|| value.strip_prefix("mcts_"))
            .or_else(|| value.strip_prefix("mut_"));
        let Some(stripped) = next else {
            break;
        };
        value = stripped.to_string();
    }
    if let Some((prefix, _)) = value.split_once("_runtime_pass_through_") {
        value = prefix.to_string();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autofactor::{AutoFactorDecision, FactorExpr};

    fn candidate_replay_identity_artifact(
        version: &str,
        target: &str,
        side: Option<ReviewSide>,
        horizon: &str,
        name: &str,
        runtime_score: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "runtime_score": runtime_score,
            "decision_contract": {
                "target": target,
                "side": side,
                "horizon": horizon,
            },
            "source_factor": {
                "version": version,
                "target": target,
                "side": side,
                "name": name,
                "dsl_hash": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "horizon": horizon,
            }
        })
    }

    #[test]
    fn candidate_replay_identity_accepts_exact_settlement_and_repricing_roots() {
        let settlement = candidate_replay_identity_artifact(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            "5m",
            "auto_settlement_conservative_settlement_edge",
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
        );
        let settlement = CandidateReplayFactorIdentity::from_artifact(&settlement)
            .expect("exact settlement identity");
        assert_eq!(settlement.side(), None);
        assert_eq!(settlement.horizon(), "5m");

        let repricing = candidate_replay_identity_artifact(
            SIDE_BOUND_ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_reprice_pnl_10s",
            Some(ReviewSide::Up),
            "10s",
            "repricing_gap_side_10s",
            "repricing_gap_side_10s",
        );
        let repricing = CandidateReplayFactorIdentity::from_artifact(&repricing)
            .expect("exact repricing identity");
        assert_eq!(repricing.side(), Some(ReviewSide::Up));
        assert_eq!(repricing.target(), "full_depth_reprice_pnl_10s");
    }

    #[test]
    fn candidate_replay_identity_rejects_ambiguous_or_spoofed_roots() {
        let valid = candidate_replay_identity_artifact(
            SIDE_BOUND_ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_reprice_pnl_10s",
            Some(ReviewSide::Up),
            "10s",
            "repricing_gap_side_10s",
            "repricing_gap_side_10s",
        );
        let mut missing_side = valid.clone();
        missing_side["source_factor"]
            .as_object_mut()
            .expect("source factor")
            .remove("side");
        let mut wrong_horizon = valid.clone();
        wrong_horizon["source_factor"]["horizon"] = serde_json::json!("5m");
        let mut wrong_runtime_score = valid.clone();
        wrong_runtime_score["runtime_score"] = serde_json::json!("autofactor_formula:other");
        let mut malformed_dsl_hash = valid.clone();
        malformed_dsl_hash["source_factor"]["dsl_hash"] = serde_json::json!("sha256:factor");
        let mut conflicting_duplicate = valid.clone();
        conflicting_duplicate["side"] = serde_json::json!("Down");
        let mut conflicting_decision_target = valid.clone();
        conflicting_decision_target["decision_contract"]["target"] =
            serde_json::json!("full_depth_reprice_pnl_30s");
        let mut conflicting_decision_horizon = valid.clone();
        conflicting_decision_horizon["decision_contract"]["horizon"] = serde_json::json!("30s");
        let mut conflicting_decision_side = valid.clone();
        conflicting_decision_side["decision_contract"]["side"] = serde_json::json!("Down");
        let legacy_pooled_repricing = candidate_replay_identity_artifact(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_reprice_pnl_10s",
            None,
            "10s",
            "repricing_gap_side_10s",
            "repricing_gap_side_10s",
        );

        for artifact in [
            missing_side,
            wrong_horizon,
            wrong_runtime_score,
            malformed_dsl_hash,
            conflicting_duplicate,
            conflicting_decision_target,
            conflicting_decision_horizon,
            conflicting_decision_side,
            legacy_pooled_repricing,
            serde_json::json!({"runtime_score": "autofactor_formula:missing_root"}),
        ] {
            CandidateReplayFactorIdentity::from_artifact(&artifact)
                .expect_err("ambiguous or spoofed identity must fail closed");
        }
    }

    fn sample_report(name: &str) -> AutoFactorReport {
        AutoFactorReport {
            name: name.to_string(),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            side: None,
            expr: FactorExpr::Input("conservative_settlement_edge".to_string()),
            n: 100,
            pearson_ic: 0.2,
            spearman_ic: 0.25,
            window_count: 3,
            window_ic_mean: 0.2,
            icir: 1.2,
            positive_window_ratio: 1.0,
            symbol_count: 2,
            symbol_ic_mean: 0.18,
            symbol_icir: 1.0,
            symbol_positive_ratio: 1.0,
            bucket_avg_labels: vec![-0.1, 0.0, 0.2],
            bottom_bucket_n: 20,
            bottom_bucket_avg_label: -0.1,
            top_bucket_n: 20,
            top_bucket_avg_label: 0.2,
            top_bucket_positive_label_rate: 0.7,
            top_bucket_full_depth_entry_fill_rate: 0.8,
            top_bucket_avg_entry_sweep_slippage_bps: 20.0,
            top_bucket_avg_entry_sweep_levels: 1.5,
            top_bucket_unique_event_count: 20,
            top_bucket_max_event_decisions: 1,
            monotonicity_score: 1.0,
            complexity: 1,
            decision: AutoFactorDecision::Candidate,
            reason: "passed".to_string(),
            parent_name: None,
        }
    }

    #[test]
    fn fifteen_second_full_depth_repricing_target_requires_side_bound_artifacts() {
        assert!(is_side_bound_repricing_target("full_depth_reprice_pnl_15s"));
        assert!(!is_side_bound_repricing_target("reprice_pnl_15s"));
    }

    fn checkpoint(
        target: &str,
        side: Option<ReviewSide>,
        nodes: Vec<FormulaMctsCheckpointNode>,
        subtree_frequencies: Vec<SubtreeFrequencyState>,
    ) -> FormulaMctsCheckpoint {
        FormulaMctsCheckpoint {
            version: FORMULA_MCTS_CHECKPOINT_VERSION.to_string(),
            target: target.to_string(),
            side,
            selection_budget: FORMULA_MCTS_SELECTION_BUDGET,
            nodes: std::iter::once(FormulaMctsCheckpointNode {
                factor_name: FORMULA_MCTS_ROOT_ID.to_string(),
                parent_name: None,
                visits: 0,
                total_reward: 0.0,
                best_reward: None,
                last_reward: 0.0,
                selected_dimension: "root".to_string(),
                last_decision: "root".to_string(),
            })
            .chain(nodes)
            .collect(),
            subtree_frequencies,
        }
    }

    #[test]
    fn structural_signature_normalizes_commutative_operands() {
        let lhs = FactorExpr::Add(
            Box::new(FactorExpr::Input("near_strike_score".to_string())),
            Box::new(FactorExpr::Mul(
                Box::new(FactorExpr::Input("entry_capacity_score".to_string())),
                Box::new(FactorExpr::Const(0.25)),
            )),
        );
        let rhs = FactorExpr::Add(
            Box::new(FactorExpr::Mul(
                Box::new(FactorExpr::Const(0.25)),
                Box::new(FactorExpr::Input("entry_capacity_score".to_string())),
            )),
            Box::new(FactorExpr::Input("near_strike_score".to_string())),
        );

        assert_eq!(structural_signature(&lhs), structural_signature(&rhs));
    }

    #[test]
    fn structural_signature_normalizes_commutative_comparators() {
        let lhs = FactorExpr::Max(
            Box::new(FactorExpr::Input("near_strike_score".to_string())),
            Box::new(FactorExpr::Min(
                Box::new(FactorExpr::Input("entry_capacity_score".to_string())),
                Box::new(FactorExpr::Const(0.25)),
            )),
        );
        let rhs = FactorExpr::Max(
            Box::new(FactorExpr::Min(
                Box::new(FactorExpr::Const(0.25)),
                Box::new(FactorExpr::Input("entry_capacity_score".to_string())),
            )),
            Box::new(FactorExpr::Input("near_strike_score".to_string())),
        );

        assert_eq!(structural_signature(&lhs), structural_signature(&rhs));
    }

    #[test]
    fn structural_signature_abstracts_numeric_constants() {
        let first = FactorExpr::SafeDiv(
            Box::new(FactorExpr::Input(
                "external_move_since_poly_update".to_string(),
            )),
            Box::new(FactorExpr::Const(0.01)),
        );
        let second = FactorExpr::SafeDiv(
            Box::new(FactorExpr::Input(
                "external_move_since_poly_update".to_string(),
            )),
            Box::new(FactorExpr::Const(0.05)),
        );

        assert_eq!(structural_signature(&first), structural_signature(&second));
    }

    #[test]
    fn avoided_subtrees_counts_repeated_inner_structures() {
        let inner = FactorExpr::SafeDiv(
            Box::new(FactorExpr::Input(
                "external_move_since_poly_update".to_string(),
            )),
            Box::new(FactorExpr::Add(
                Box::new(FactorExpr::Input("pm_spread".to_string())),
                Box::new(FactorExpr::Const(0.01)),
            )),
        );
        let reports = vec![
            AutoFactorReport {
                name: "wrapped_tanh".to_string(),
                expr: FactorExpr::Tanh(Box::new(inner.clone())),
                complexity: 5,
                ..sample_report("wrapped_tanh")
            },
            AutoFactorReport {
                name: "wrapped_log".to_string(),
                expr: FactorExpr::Log1pAbs(Box::new(inner.clone())),
                complexity: 5,
                ..sample_report("wrapped_log")
            },
            AutoFactorReport {
                name: "wrapped_sqrt".to_string(),
                expr: FactorExpr::SqrtAbs(Box::new(inner.clone())),
                complexity: 5,
                ..sample_report("wrapped_sqrt")
            },
        ];

        let frequencies =
            subtree_frequency_state("full_depth_settlement_executable_pnl", &reports, None, None);
        let avoided = avoided_subtrees(None, &frequencies);
        let inner_signature = structural_signature(&inner);
        let subtree = avoided
            .iter()
            .find(|item| item.structural_signature == inner_signature)
            .expect("shared inner subtree is crowded");

        assert_eq!(subtree.root_gene, "SafeDiv");
        assert_eq!(subtree.count, 3);
        assert_eq!(subtree.reason, "structural_signature_crowding");
    }

    #[test]
    fn subtree_frequencies_merge_prior_state_counts() {
        let expr = FactorExpr::SafeDiv(
            Box::new(FactorExpr::Input(
                "external_move_since_poly_update".to_string(),
            )),
            Box::new(FactorExpr::Const(0.01)),
        );
        let report = AutoFactorReport {
            name: "current_safe_div".to_string(),
            expr: expr.clone(),
            complexity: 3,
            ..sample_report("current_safe_div")
        };
        let signature = structural_signature(&expr);
        let prior = checkpoint(
            "full_depth_settlement_executable_pnl",
            None,
            Vec::new(),
            vec![SubtreeFrequencyState {
                root_gene: "SafeDiv".to_string(),
                structural_signature: signature.clone(),
                depth: structural_depth(&expr),
                count: 2,
            }],
        );

        let frequencies = subtree_frequency_state(
            "full_depth_settlement_executable_pnl",
            &[report],
            Some(&prior),
            None,
        );
        let item = frequencies
            .iter()
            .find(|item| item.structural_signature == signature)
            .expect("merged signature");

        assert_eq!(item.count, 3);
    }

    #[test]
    fn crowded_signature_receives_reward_penalty() {
        let expr = FactorExpr::SafeDiv(
            Box::new(FactorExpr::Input(
                "external_move_since_poly_update".to_string(),
            )),
            Box::new(FactorExpr::Const(0.01)),
        );
        let mut crowded = sample_report("crowded_safe_div");
        crowded.expr = expr.clone();
        crowded.complexity = 3;
        let mut novel = sample_report("novel_tanh");
        novel.expr = FactorExpr::Tanh(Box::new(FactorExpr::Input("near_strike_score".to_string())));
        novel.complexity = 3;
        let frequencies = vec![SubtreeFrequencyState {
            root_gene: "SafeDiv".to_string(),
            structural_signature: structural_signature(&expr),
            depth: structural_depth(&expr),
            count: 5,
        }];
        let runtime_avoidances = Vec::new();

        assert!(
            structural_diversity_penalty(&crowded.expr, &frequencies) > 0.0,
            "crowded expression should be penalized"
        );
        assert!(
            reward(&crowded, &runtime_avoidances, None, &frequencies)
                < reward(&novel, &runtime_avoidances, None, &frequencies),
            "same-quality crowded expression should rank below novel expression"
        );
    }

    #[test]
    fn writes_search_artifact_bundle() {
        let tmp =
            std::env::temp_dir().join(format!("ploy-alpha-search-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let report = AutoFactorReport {
            name: "auto_settlement_conservative_settlement_edge".to_string(),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            side: None,
            expr: FactorExpr::Input("conservative_settlement_edge".to_string()),
            n: 100,
            pearson_ic: 0.2,
            spearman_ic: 0.25,
            window_count: 3,
            window_ic_mean: 0.2,
            icir: 1.2,
            positive_window_ratio: 1.0,
            symbol_count: 2,
            symbol_ic_mean: 0.18,
            symbol_icir: 1.0,
            symbol_positive_ratio: 1.0,
            bucket_avg_labels: vec![-0.1, 0.0, 0.2],
            bottom_bucket_n: 20,
            bottom_bucket_avg_label: -0.1,
            top_bucket_n: 20,
            top_bucket_avg_label: 0.2,
            top_bucket_positive_label_rate: 0.7,
            top_bucket_full_depth_entry_fill_rate: 0.8,
            top_bucket_avg_entry_sweep_slippage_bps: 20.0,
            top_bucket_avg_entry_sweep_levels: 1.5,
            top_bucket_unique_event_count: 20,
            top_bucket_max_event_decisions: 1,
            monotonicity_score: 1.0,
            complexity: 1,
            decision: AutoFactorDecision::Candidate,
            reason: "passed".to_string(),
            parent_name: None,
        };
        let summary = write_alpha_search_artifacts(
            &tmp,
            "full_depth_settlement_executable_pnl",
            &["conservative_settlement_edge".to_string()],
            &[report],
            &AutoFactorOptions::default(),
        )
        .expect("write artifacts");
        assert_eq!(summary.candidate_count, 1);
        assert!(tmp
            .join("full_depth_settlement_executable_pnl/search-space.json")
            .exists());
        assert!(tmp
            .join("full_depth_settlement_executable_pnl/tree-trace.json")
            .exists());
        assert!(tmp
            .join("full_depth_settlement_executable_pnl/mcts-expansion-plan.json")
            .exists());
        assert!(tmp
            .join("full_depth_settlement_executable_pnl/mcts-state.json")
            .exists());
        let registry_preview =
            tmp.join("full_depth_settlement_executable_pnl/factor-registry-preview.json");
        assert!(registry_preview.exists());
        let preview: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(registry_preview).expect("read preview"))
                .expect("preview json");
        assert_eq!(preview["version"], ALPHA_SEARCH_ARTIFACT_VERSION);
        assert_eq!(preview["target"], "full_depth_settlement_executable_pnl");
        assert!(preview.get("side").is_none());
        assert_eq!(preview["horizon"], "5m");
        let rows = preview["factors"].as_array().expect("factors array");
        assert_eq!(
            rows[0]["factor_name"],
            "auto_settlement_conservative_settlement_edge"
        );
        assert_eq!(rows[0]["horizon"], "5m");
        assert_eq!(rows[0]["status"], "candidate");
        assert!(rows[0]["dsl_hash"].as_str().expect("dsl hash").len() >= 32);
        assert_eq!(
            rows[0]["runtime_contract"]["runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge"
        );
        assert_eq!(
            rows[0]["runtime_contract"]["strategy_profile"],
            "settlement_probability"
        );
        assert_eq!(
            rows[0]["runtime_contract"]["input_names"],
            serde_json::json!(["conservative_settlement_edge"])
        );
        assert_eq!(
            rows[0]["runtime_contract"]["ast_input_names"],
            serde_json::json!(["conservative_settlement_edge"])
        );
        assert_eq!(
            rows[0]["runtime_contract"]["runtime_input_names"],
            serde_json::json!(["settlement_edge"])
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn side_bound_artifacts_do_not_overwrite_and_reject_wrong_side_state() {
        let temp = tempfile::tempdir().expect("create isolated artifact directory");
        let tmp = temp.path();
        let target = "full_depth_reprice_pnl_10s";
        let report_for = |side| AutoFactorReport {
            target: Some(target.to_string()),
            side: Some(side),
            expr: FactorExpr::Input("repricing_gap_side_10s".to_string()),
            ..sample_report("repricing_gap_side_10s")
        };
        let write = |root: &Path,
                     side,
                     report: &AutoFactorReport,
                     state: Option<&FormulaMctsCheckpoint>| {
            write_side_bound_alpha_search_artifacts_with_state_and_runtime_feedback(
                root,
                target,
                side,
                &["repricing_gap_side_10s".to_string()],
                std::slice::from_ref(report),
                &AutoFactorOptions::default(),
                state,
                None,
                None,
            )
        };

        let up = report_for(ReviewSide::Up);
        let up_summary = write(tmp, ReviewSide::Up, &up, None).expect("write Up artifacts");
        let up_search_space = tmp.join(target).join("up/search-space.json");
        let up_before = std::fs::read(&up_search_space).expect("read Up search space");

        let down = report_for(ReviewSide::Down);
        write(tmp, ReviewSide::Down, &down, None).expect("write Down artifacts");

        assert_eq!(up_summary.side, Some(ReviewSide::Up));
        assert_eq!(
            std::fs::read(&up_search_space).expect("reread Up search space"),
            up_before
        );
        assert!(tmp.join(target).join("down/search-space.json").exists());

        let pooled_root = tmp.join("pooled");
        let err = write_alpha_search_artifacts(
            &pooled_root,
            target,
            &["repricing_gap_side_10s".to_string()],
            std::slice::from_ref(&up),
            &AutoFactorOptions::default(),
        )
        .expect_err("side-bound reports must not use the pooled writer");
        assert!(matches!(err, AlphaSearchArtifactError::IdentityMismatch(_)));
        assert!(!pooled_root.join(target).exists());

        let wrong_side_state = checkpoint(target, Some(ReviewSide::Down), Vec::new(), Vec::new());
        let mismatch_root = tmp.join("mismatch");
        let err = write(&mismatch_root, ReviewSide::Up, &up, Some(&wrong_side_state))
            .expect_err("wrong-side prior state must fail closed");
        assert!(matches!(err, AlphaSearchArtifactError::IdentityMismatch(_)));
        assert!(!mismatch_root.join(target).join("up").exists());
    }

    #[test]
    fn runtime_contract_canonicalizes_supported_research_inputs() {
        let report = AutoFactorReport {
            name: "auto_settlement_model_full_depth_settlement_edge_x_near_strike_x_capacity"
                .to_string(),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            expr: FactorExpr::Mul(
                Box::new(FactorExpr::Mul(
                    Box::new(FactorExpr::Input(
                        "model_full_depth_settlement_edge".to_string(),
                    )),
                    Box::new(FactorExpr::Input("near_strike_score".to_string())),
                )),
                Box::new(FactorExpr::Input("entry_capacity_score".to_string())),
            ),
            ..sample_report(
                "auto_settlement_model_full_depth_settlement_edge_x_near_strike_x_capacity",
            )
        };
        let contract =
            runtime_contract_for_report(&report, "dsl-hash", &serde_json::json!({}), "5m");
        assert_eq!(
            contract["runtime_score"],
            "autofactor_formula:auto_settlement_model_full_depth_settlement_edge_x_near_strike_x_capacity"
        );
        assert_eq!(
            contract["ast_input_names"],
            serde_json::json!([
                "entry_capacity_score",
                "model_full_depth_settlement_edge",
                "near_strike_score"
            ])
        );
        assert_eq!(
            contract["runtime_input_names"],
            serde_json::json!([
                "direction_sign",
                "distance_over_sigma",
                "entry_capacity_ratio",
                "settlement_edge"
            ])
        );
        assert_eq!(contract["blockers"], serde_json::json!([]));
    }

    #[test]
    fn runtime_contract_maps_settlement_selector_threshold_formulas() {
        let report = AutoFactorReport {
            name: "auto_settlement_model_conservative_settlement_edge_x_capacity_select_entry_price_quality_ge_025"
                .to_string(),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            expr: FactorExpr::Gate {
                expr: Box::new(FactorExpr::Mul(
                    Box::new(FactorExpr::Input(
                        "model_conservative_settlement_edge".to_string(),
                    )),
                    Box::new(FactorExpr::Input("entry_capacity_score".to_string())),
                )),
                gate: Box::new(FactorExpr::Input("entry_price_quality_score".to_string())),
                min: 0.25,
            },
            ..sample_report(
                "auto_settlement_model_conservative_settlement_edge_x_capacity_select_entry_price_quality_ge_025",
            )
        };
        let contract =
            runtime_contract_for_report(&report, "dsl-hash", &serde_json::json!({}), "5m");
        assert_eq!(
            contract["runtime_score"],
            "autofactor_formula:auto_settlement_model_conservative_settlement_edge_x_capacity_select_entry_price_quality_ge_025"
        );
        assert_eq!(
            contract["runtime_input_names"],
            serde_json::json!(["entry_capacity_ratio", "entry_price", "settlement_edge"])
        );
        assert_eq!(contract["blockers"], serde_json::json!([]));
    }

    #[test]
    fn runtime_contract_rejects_malformed_settlement_selector_threshold() {
        let report = AutoFactorReport {
            name: "auto_settlement_model_conservative_settlement_edge_x_capacity_select_entry_price_quality_ge_bad"
                .to_string(),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            expr: FactorExpr::Gate {
                expr: Box::new(FactorExpr::Mul(
                    Box::new(FactorExpr::Input(
                        "model_conservative_settlement_edge".to_string(),
                    )),
                    Box::new(FactorExpr::Input("entry_capacity_score".to_string())),
                )),
                gate: Box::new(FactorExpr::Input("entry_price_quality_score".to_string())),
                min: 0.25,
            },
            ..sample_report(
                "auto_settlement_model_conservative_settlement_edge_x_capacity_select_entry_price_quality_ge_bad",
            )
        };
        let contract =
            runtime_contract_for_report(&report, "dsl-hash", &serde_json::json!({}), "5m");
        assert_eq!(contract["runtime_score"], "");
        let blockers = contract["blockers"].as_array().expect("blockers");
        assert!(blockers
            .iter()
            .any(|item| item.as_str() == Some("runtime_contract_unmapped_factor")));
    }

    #[test]
    fn runtime_contract_blocks_noncanonical_runtime_inputs() {
        let external_pressure_report = AutoFactorReport {
            name: "auto_settlement_model_full_depth_settlement_edge_x_external_pressure"
                .to_string(),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            expr: FactorExpr::Mul(
                Box::new(FactorExpr::Input(
                    "model_full_depth_settlement_edge".to_string(),
                )),
                Box::new(FactorExpr::Input("external_pressure".to_string())),
            ),
            ..sample_report("auto_settlement_model_full_depth_settlement_edge_x_external_pressure")
        };
        let contract = runtime_contract_for_report(
            &external_pressure_report,
            "dsl-hash",
            &serde_json::json!({}),
            "5m",
        );
        let blockers = contract["blockers"].as_array().expect("blockers");
        assert!(blockers.iter().any(|item| {
            item.as_str() == Some("runtime_input_semantics_mismatch:external_pressure")
        }));

        let iv_change_report = AutoFactorReport {
            name: "auto_settlement_conservative_settlement_edge_x_iv_change".to_string(),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            expr: FactorExpr::Mul(
                Box::new(FactorExpr::Input(
                    "conservative_settlement_edge".to_string(),
                )),
                Box::new(FactorExpr::Input("iv_change_1m".to_string())),
            ),
            ..sample_report("auto_settlement_conservative_settlement_edge_x_iv_change")
        };
        let contract = runtime_contract_for_report(
            &iv_change_report,
            "dsl-hash",
            &serde_json::json!({}),
            "5m",
        );
        let blockers = contract["blockers"].as_array().expect("blockers");
        assert!(blockers
            .iter()
            .any(|item| item.as_str() == Some("runtime_input_not_supplied:iv_change_1m")));
    }

    #[test]
    fn runtime_contract_blocks_unsupported_predictive_suffixes() {
        let unsupported = sample_report("llm_amplitude_weighted_momentum_30s_sigma_feature_gate");
        let contract =
            runtime_contract_for_report(&unsupported, "dsl-hash", &serde_json::json!({}), "5m");
        assert_eq!(contract["runtime_score"], "");
        let blockers = contract["blockers"].as_array().expect("blockers");
        assert!(blockers
            .iter()
            .any(|item| item.as_str() == Some("runtime_contract_unmapped_factor")));
        assert!(blockers.iter().any(|item| {
            item.as_str() == Some("runtime_contract_unsupported_predictive_suffix")
        }));
    }

    #[test]
    fn merges_prior_mcts_state_into_search_artifacts() {
        let tmp = std::env::temp_dir().join(format!(
            "ploy-alpha-search-state-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let report = AutoFactorReport {
            name: "auto_settlement_conservative_settlement_edge".to_string(),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            side: None,
            expr: FactorExpr::Input("conservative_settlement_edge".to_string()),
            n: 100,
            pearson_ic: 0.2,
            spearman_ic: 0.25,
            window_count: 3,
            window_ic_mean: 0.2,
            icir: 1.2,
            positive_window_ratio: 1.0,
            symbol_count: 2,
            symbol_ic_mean: 0.18,
            symbol_icir: 1.0,
            symbol_positive_ratio: 1.0,
            bucket_avg_labels: vec![-0.1, 0.0, 0.2],
            bottom_bucket_n: 20,
            bottom_bucket_avg_label: -0.1,
            top_bucket_n: 20,
            top_bucket_avg_label: 0.2,
            top_bucket_positive_label_rate: 0.7,
            top_bucket_full_depth_entry_fill_rate: 0.8,
            top_bucket_avg_entry_sweep_slippage_bps: 20.0,
            top_bucket_avg_entry_sweep_levels: 1.5,
            top_bucket_unique_event_count: 20,
            top_bucket_max_event_decisions: 1,
            monotonicity_score: 1.0,
            complexity: 1,
            decision: AutoFactorDecision::Candidate,
            reason: "passed".to_string(),
            parent_name: None,
        };
        let mut prior = checkpoint(
            "full_depth_settlement_executable_pnl",
            None,
            vec![FormulaMctsCheckpointNode {
                factor_name: "auto_settlement_conservative_settlement_edge".to_string(),
                parent_name: None,
                visits: 3,
                total_reward: 6.0,
                best_reward: Some(2.0),
                last_reward: 2.0,
                selected_dimension: "exploit".to_string(),
                last_decision: "candidate".to_string(),
            }],
            Vec::new(),
        );
        let root = prior.nodes.first_mut().expect("checkpoint root");
        root.visits = 3;
        root.total_reward = 6.0;
        root.best_reward = Some(2.0);

        write_alpha_search_artifacts_with_state(
            &tmp,
            "full_depth_settlement_executable_pnl",
            &["conservative_settlement_edge".to_string()],
            &[report],
            &AutoFactorOptions::default(),
            Some(&prior),
        )
        .expect("write artifacts");

        let state = read_mcts_search_state(
            tmp.join("full_depth_settlement_executable_pnl/mcts-state.json"),
        )
        .expect("state");
        let node = state
            .nodes
            .iter()
            .find(|node| node.factor_name == "auto_settlement_conservative_settlement_edge")
            .expect("merged node");
        assert_eq!(node.visits, 4);
        assert!(node.total_reward > 6.0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn mcts_state_backpropagates_leaf_reward_to_ancestors() {
        let runtime_avoidances = Vec::new();
        let root = sample_report("root_factor");
        let mut sibling = sample_report("sibling_factor");
        sibling.top_bucket_avg_label = 0.15;
        let mut child = sample_report("mut_root_factor_capacity");
        child.parent_name = Some(root.name.clone());
        child.top_bucket_avg_label = 0.30;
        let mut grandchild = sample_report("mut2_root_factor_capacity_squashed");
        grandchild.parent_name = Some(child.name.clone());
        grandchild.top_bucket_avg_label = 0.40;

        let reports = [root, sibling, child, grandchild];
        let metrics = reports
            .iter()
            .enumerate()
            .map(|(idx, report)| node_metric(idx, report, &runtime_avoidances, None, &Vec::new()))
            .collect::<Vec<_>>();
        let reward_by_name = metrics
            .iter()
            .map(|metric| (metric.factor_name.as_str(), metric.reward))
            .collect::<BTreeMap<_, _>>();
        let state = mcts_search_state(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics,
            None,
            Vec::new(),
        );
        let nodes = state
            .nodes
            .iter()
            .map(|node| (node.factor_name.as_str(), node))
            .collect::<BTreeMap<_, _>>();

        let root_node = nodes.get("root_factor").expect("root node");
        let child_node = nodes.get("mut_root_factor_capacity").expect("child node");
        let grandchild_node = nodes
            .get("mut2_root_factor_capacity_squashed")
            .expect("grandchild node");
        let sibling_node = nodes.get("sibling_factor").expect("sibling node");

        assert_eq!(root_node.visits, 3);
        assert_eq!(child_node.visits, 2);
        assert_eq!(grandchild_node.visits, 1);
        assert_eq!(sibling_node.visits, 1);
        assert_eq!(
            root_node.total_reward,
            reward_by_name["root_factor"]
                + reward_by_name["mut_root_factor_capacity"]
                + reward_by_name["mut2_root_factor_capacity_squashed"]
        );
        assert_eq!(
            child_node.total_reward,
            reward_by_name["mut_root_factor_capacity"]
                + reward_by_name["mut2_root_factor_capacity_squashed"]
        );
        assert_eq!(sibling_node.total_reward, reward_by_name["sibling_factor"]);
    }

    #[test]
    fn mcts_state_backpropagates_through_long_lineage() {
        let runtime_avoidances = Vec::new();
        let mut reports = Vec::new();
        for idx in 0..100 {
            let mut report = sample_report(&format!("factor_{idx:03}"));
            if idx > 0 {
                report.parent_name = Some(format!("factor_{:03}", idx - 1));
            }
            report.top_bucket_avg_label = 0.20 + (idx as f64 * 0.001);
            reports.push(report);
        }

        let metrics = reports
            .iter()
            .enumerate()
            .map(|(idx, report)| node_metric(idx, report, &runtime_avoidances, None, &Vec::new()))
            .collect::<Vec<_>>();
        let expected_root_total = metrics.iter().map(|metric| metric.reward).sum::<f64>();
        let state = mcts_search_state(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics,
            None,
            Vec::new(),
        );
        let root = state
            .nodes
            .iter()
            .find(|node| node.factor_name == "factor_000")
            .expect("root node");

        assert_eq!(state.backpropagation_truncated_count, 0);
        assert_eq!(root.visits, 100);
        assert!((root.total_reward - expected_root_total).abs() < 1e-9);
    }

    #[test]
    fn formula_mcts_checkpoint_rejects_cyclic_lineage() {
        let runtime_avoidances = Vec::new();
        let mut report = sample_report("cycle_a");
        report.parent_name = Some("cycle_b".to_string());
        let metrics = [node_metric(
            0,
            &report,
            &runtime_avoidances,
            None,
            &Vec::new(),
        )];
        let prior = checkpoint(
            "full_depth_settlement_executable_pnl",
            None,
            vec![FormulaMctsCheckpointNode {
                factor_name: "cycle_b".to_string(),
                parent_name: Some("cycle_a".to_string()),
                visits: 0,
                total_reward: 0.0,
                best_reward: None,
                last_reward: 0.0,
                selected_dimension: "exploit".to_string(),
                last_decision: "candidate".to_string(),
            }],
            Vec::new(),
        );

        let err = formula_mcts_checkpoint(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics,
            Some(&prior),
            Vec::new(),
        )
        .expect_err("cyclic Formula candidate lineage must fail closed");

        assert!(matches!(err, AlphaSearchArtifactError::IdentityMismatch(_)));
    }

    #[test]
    fn formula_mcts_checkpoint_resume_matches_uninterrupted_run() {
        let runtime_avoidances = Vec::new();
        let root = sample_report("resume_root");
        let mut child = sample_report("resume_child");
        child.parent_name = Some(root.name.clone());
        let metrics = [root, child]
            .iter()
            .enumerate()
            .map(|(idx, report)| node_metric(idx, report, &runtime_avoidances, None, &Vec::new()))
            .collect::<Vec<_>>();
        let uninterrupted = formula_mcts_checkpoint(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics,
            None,
            Vec::new(),
        )
        .expect("uninterrupted checkpoint");
        let first = formula_mcts_checkpoint(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics[..1],
            None,
            Vec::new(),
        )
        .expect("first checkpoint");
        let resumed = formula_mcts_checkpoint(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics[1..],
            Some(&first),
            Vec::new(),
        )
        .expect("resumed checkpoint");

        assert_eq!(
            serde_json::to_vec(&resumed).expect("serialize resumed checkpoint"),
            serde_json::to_vec(&uninterrupted).expect("serialize uninterrupted checkpoint")
        );
    }

    #[test]
    fn formula_mcts_checkpoint_rejects_forged_target_and_legacy_state() {
        let forged = checkpoint("other_target", None, Vec::new(), Vec::new());
        let err = formula_mcts_checkpoint(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &[],
            Some(&forged),
            Vec::new(),
        )
        .expect_err("forged target must not resume Formula search");
        assert!(matches!(err, AlphaSearchArtifactError::IdentityMismatch(_)));

        let mut altered_budget = checkpoint(
            "full_depth_settlement_executable_pnl",
            None,
            Vec::new(),
            Vec::new(),
        );
        altered_budget.selection_budget = FORMULA_MCTS_SELECTION_BUDGET + 1;
        let err = formula_mcts_checkpoint(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &[],
            Some(&altered_budget),
            Vec::new(),
        )
        .expect_err("altered selection budget must not resume Formula search");
        assert!(matches!(err, AlphaSearchArtifactError::IdentityMismatch(_)));

        let mut reordered = checkpoint(
            "full_depth_settlement_executable_pnl",
            None,
            vec![FormulaMctsCheckpointNode {
                factor_name: "forged_candidate".to_string(),
                parent_name: None,
                visits: 1,
                total_reward: 1.0,
                best_reward: Some(1.0),
                last_reward: 1.0,
                selected_dimension: "exploit".to_string(),
                last_decision: "candidate".to_string(),
            }],
            Vec::new(),
        );
        reordered.nodes.swap(0, 1);
        let err = mcts_search_state_projection(&reordered)
            .expect_err("reordered checkpoint must not project a wrong root budget");
        assert!(matches!(err, AlphaSearchArtifactError::IdentityMismatch(_)));

        let temp = tempfile::tempdir().expect("temporary legacy state");
        let path = temp.path().join("mcts-state.json");
        std::fs::write(
            &path,
            r#"{"version":"alpha_search_artifacts_v1","target":"full_depth_settlement_executable_pnl"}"#,
        )
        .expect("write legacy state");
        let err = read_formula_mcts_checkpoint(&path)
            .expect_err("legacy projection must not be reinterpreted as a checkpoint");
        assert!(matches!(
            err,
            AlphaSearchArtifactError::LegacyCheckpointVersion(ref version)
                if version == ALPHA_SEARCH_ARTIFACT_VERSION
        ));
    }

    #[test]
    fn repeated_event_candidate_ranks_below_one_event_candidate() {
        let mut repeated = sample_report("auto_settlement_high_raw_score_repeated_event");
        repeated.spearman_ic = 0.95;
        repeated.icir = 3.0;
        repeated.top_bucket_avg_label = 0.7;
        repeated.top_bucket_unique_event_count = 4;
        repeated.top_bucket_max_event_decisions = 5;

        let mut one_event = sample_report("auto_settlement_lower_raw_score_one_event");
        one_event.spearman_ic = 0.35;
        one_event.icir = 0.9;
        one_event.top_bucket_avg_label = 0.25;
        one_event.top_bucket_unique_event_count = one_event.top_bucket_n;
        one_event.top_bucket_max_event_decisions = 1;

        let reports = [repeated, one_event];
        let runtime_avoidances = Vec::new();
        let subtree_frequencies = Vec::new();
        let metrics = reports
            .iter()
            .enumerate()
            .map(|(idx, report)| {
                node_metric(idx, report, &runtime_avoidances, None, &subtree_frequencies)
            })
            .collect::<Vec<_>>();
        let state = mcts_search_state(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics,
            None,
            subtree_frequencies.clone(),
        );
        let plan = mcts_expansion_plan(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics,
            &state,
        );

        assert_eq!(
            plan.selected_nodes
                .first()
                .map(|node| node.factor_name.as_str()),
            Some("auto_settlement_lower_raw_score_one_event")
        );
        assert!(
            reward(&reports[0], &runtime_avoidances, None, &subtree_frequencies)
                < reward(&reports[1], &runtime_avoidances, None, &subtree_frequencies),
            "repeated-event penalty should dominate raw IC/top-bucket strength"
        );
    }

    #[test]
    fn repeated_event_candidate_selects_event_uniqueness_mutation() {
        let mut report = sample_report("auto_settlement_repeated_event_branch");
        report.top_bucket_unique_event_count = 6;
        report.top_bucket_max_event_decisions = 3;
        report.reason = "passed".to_string();

        let runtime_avoidances = Vec::new();
        assert_eq!(
            selected_dimension(&report, &runtime_avoidances),
            "event_uniqueness"
        );
        assert_eq!(
            proposed_mutation(&selected_dimension(&report, &runtime_avoidances)),
            "add_capacity_gate"
        );
    }

    #[test]
    fn high_sweep_slippage_selects_execution_quality() {
        let mut report = sample_report("auto_settlement_high_sweep_slippage");
        report.top_bucket_avg_entry_sweep_slippage_bps = 450.0;
        report.top_bucket_avg_entry_sweep_levels = 3.4;
        report.reason = "passed".to_string();

        let runtime_avoidances = Vec::new();
        assert_eq!(
            selected_dimension(&report, &runtime_avoidances),
            "execution_quality"
        );
        assert!(execution_penalty(&report) > 0.0);
    }

    #[test]
    fn runtime_pass_through_collapse_penalizes_matching_factor_family() {
        let mut collapsed = sample_report("mut_spread_adjusted_external_move_near_strike");
        collapsed.spearman_ic = 0.95;
        collapsed.icir = 3.0;
        collapsed.top_bucket_avg_label = 3.0;

        let mut alternative =
            sample_report("auto_settlement_full_depth_settlement_edge_x_capacity");
        alternative.spearman_ic = 0.25;
        alternative.icir = 0.9;
        alternative.top_bucket_avg_label = 0.35;

        let feedback = AlphaSearchRuntimeFeedback {
            version: None,
            target: None,
            side: None,
            dsl_hash: None,
            runtime_score: "autofactor_formula:mut_spread_adjusted_external_move_near_strike"
                .to_string(),
            base_factor: "mut_spread_adjusted_external_move_near_strike".to_string(),
            entry_signals: 0,
            direct_passes_at_configured_threshold: 146,
            formula_evaluations: 2934,
            depth_fillable: 2934,
            executable_edge_pass_min_edge: 5,
        };
        let runtime_avoidances = runtime_avoidances(Some(&feedback), None);
        let subtree_frequencies = Vec::new();

        assert_eq!(
            selected_dimension(&collapsed, &runtime_avoidances),
            "runtime_executable_entry"
        );
        assert!(
            reward(&collapsed, &runtime_avoidances, None, &subtree_frequencies)
                < reward(
                    &alternative,
                    &runtime_avoidances,
                    None,
                    &subtree_frequencies
                ),
            "runtime pass-through collapse should dominate top-bucket reward"
        );
    }

    #[test]
    fn candidate_replay_feedback_requires_matching_dsl_hash() {
        let mut report = sample_report("mut_spread_adjusted_external_move_near_strike");
        report.spearman_ic = 0.95;
        report.icir = 3.0;
        report.top_bucket_avg_label = 3.0;
        let feedback = AlphaSearchRuntimeFeedback {
            version: Some(ALPHA_SEARCH_ARTIFACT_VERSION.to_string()),
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            side: None,
            dsl_hash: Some(
                "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            ),
            runtime_score: "autofactor_formula:mut_spread_adjusted_external_move_near_strike"
                .to_string(),
            base_factor: "mut_spread_adjusted_external_move_near_strike".to_string(),
            entry_signals: 0,
            direct_passes_at_configured_threshold: 146,
            formula_evaluations: 2934,
            depth_fillable: 2934,
            executable_edge_pass_min_edge: 5,
        };
        let temp = tempfile::tempdir().expect("candidate replay output");

        write_alpha_search_artifacts_with_state_and_runtime_feedback(
            temp.path(),
            "full_depth_settlement_executable_pnl",
            &["conservative_settlement_edge".to_string()],
            &[report],
            &AutoFactorOptions::default(),
            None,
            Some(&feedback),
            None,
            None,
        )
        .expect("write artifacts with mismatched replay hash");

        let metrics: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                temp.path()
                    .join("full_depth_settlement_executable_pnl/node-metrics.json"),
            )
            .expect("read node metrics"),
        )
        .expect("parse node metrics");
        assert_eq!(metrics[0]["runtime_pass_through_penalty"], 0.0);
    }

    #[test]
    fn runtime_pass_through_collapse_filters_mcts_expansion_nodes() {
        let mut collapsed = sample_report("mcts_spread_adjusted_external_move_near_strike");
        collapsed.spearman_ic = 0.95;
        collapsed.icir = 3.0;
        collapsed.top_bucket_avg_label = 3.0;

        let alternative = sample_report("auto_settlement_full_depth_settlement_edge_x_capacity");
        let feedback = AlphaSearchRuntimeFeedback {
            version: None,
            target: None,
            side: None,
            dsl_hash: None,
            runtime_score: "autofactor_formula:mut_spread_adjusted_external_move_near_strike"
                .to_string(),
            base_factor: "mut_spread_adjusted_external_move_near_strike".to_string(),
            entry_signals: 0,
            direct_passes_at_configured_threshold: 146,
            formula_evaluations: 2934,
            depth_fillable: 2934,
            executable_edge_pass_min_edge: 5,
        };
        let runtime_avoidances = runtime_avoidances(Some(&feedback), None);
        let reports = [collapsed, alternative];
        let subtree_frequencies = Vec::new();
        let metrics = reports
            .iter()
            .enumerate()
            .map(|(idx, report)| {
                node_metric(idx, report, &runtime_avoidances, None, &subtree_frequencies)
            })
            .collect::<Vec<_>>();
        let state = mcts_search_state(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics,
            None,
            subtree_frequencies,
        );
        let plan = mcts_expansion_plan(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics,
            &state,
        );

        assert!(!plan
            .selected_nodes
            .iter()
            .any(|node| node.factor_name == "mcts_spread_adjusted_external_move_near_strike"));
        assert_eq!(
            plan.selected_nodes
                .first()
                .map(|node| node.factor_name.as_str()),
            Some("auto_settlement_full_depth_settlement_edge_x_capacity")
        );
    }

    #[test]
    fn typed_prior_runtime_avoid_list_filters_failed_family_variants() {
        let mut squashed =
            sample_report("llm_mut_spread_adjusted_external_move_squashed_add_capacity_gate");
        squashed.spearman_ic = 0.95;
        squashed.icir = 3.0;
        squashed.top_bucket_avg_label = 3.0;

        let mut spread_adjusted =
            sample_report("mcts_spread_adjusted_external_move_spread_adjusted_entry_price_quality");
        spread_adjusted.spearman_ic = 0.90;
        spread_adjusted.icir = 2.5;
        spread_adjusted.top_bucket_avg_label = 2.5;

        let alternative = sample_report("auto_settlement_full_depth_settlement_edge_x_capacity");
        let prior = LlmPriorSpec {
            mutations: Vec::new(),
            structural_avoid_signatures: Vec::new(),
            runtime_avoid_factors: vec![crate::autofactor::RuntimeAvoidFactorSpec {
                base_factor: "mut_spread_adjusted_external_move_squashed".to_string(),
                factor_family: Some("spread_adjusted_external_move".to_string()),
                runtime_score: Some(
                    "autofactor_formula:mut_spread_adjusted_external_move_squashed".to_string(),
                ),
                reason: Some("runtime_pass_through_collapse".to_string()),
                metrics: serde_json::Value::Null,
            }],
            ..Default::default()
        };
        let prior_avoidances = runtime_avoidances(None, Some(&prior));
        let reports = [squashed, spread_adjusted, alternative];
        let subtree_frequencies = Vec::new();
        let metrics = reports
            .iter()
            .enumerate()
            .map(|(idx, report)| {
                node_metric(idx, report, &prior_avoidances, None, &subtree_frequencies)
            })
            .collect::<Vec<_>>();
        let state = mcts_search_state(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics,
            None,
            subtree_frequencies,
        );
        let plan = mcts_expansion_plan(
            ALPHA_SEARCH_ARTIFACT_VERSION,
            "full_depth_settlement_executable_pnl",
            None,
            &metrics,
            &state,
        );

        assert!(plan
            .selected_nodes
            .iter()
            .all(|node| !node.factor_name.contains("spread_adjusted_external_move")));
        assert_eq!(
            plan.selected_nodes
                .first()
                .map(|node| node.factor_name.as_str()),
            Some("auto_settlement_full_depth_settlement_edge_x_capacity")
        );

        let dangling_interaction = sample_report(
            "mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_x_full_depth_entry_gate_spread_adjusted",
        );
        let composed_prior = LlmPriorSpec {
            mutations: Vec::new(),
            structural_avoid_signatures: Vec::new(),
            runtime_avoid_factors: vec![crate::autofactor::RuntimeAvoidFactorSpec {
                base_factor:
                    "mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted"
                        .to_string(),
                factor_family: Some(
                    "auto_settlement_model_full_depth_settlement_edge_x_external_pressure"
                        .to_string(),
                ),
                runtime_score: Some(
                    "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted"
                        .to_string(),
                ),
                reason: Some("runtime_pass_through_collapse".to_string()),
                metrics: serde_json::Value::Null,
            }],
            ..Default::default()
        };
        let composed_avoidances = runtime_avoidances(None, Some(&composed_prior));
        assert_eq!(
            normalized_factor_family(&dangling_interaction.name),
            "auto_settlement_model_full_depth_settlement_edge_x_external_pressure"
        );
        assert!(matching_runtime_avoidance(&dangling_interaction, &composed_avoidances).is_some());

        let selected_gate_variant = sample_report(
            "mut_auto_settlement_model_full_depth_settlement_edge_spread_adjusted_select_near_strike_ge_025",
        );
        assert_eq!(
            normalized_factor_family(&selected_gate_variant.name),
            "auto_settlement_model_full_depth_settlement_edge"
        );
        let selected_gate_prior = LlmPriorSpec {
            mutations: Vec::new(),
            structural_avoid_signatures: Vec::new(),
            runtime_avoid_factors: vec![crate::autofactor::RuntimeAvoidFactorSpec {
                base_factor:
                    "mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted"
                        .to_string(),
                factor_family: Some("auto_settlement_model_full_depth_settlement_edge".to_string()),
                runtime_score: Some(
                    "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted"
                        .to_string(),
                ),
                reason: Some("negative_runtime_edge".to_string()),
                metrics: serde_json::Value::Null,
            }],
            ..Default::default()
        };
        assert!(matching_runtime_avoidance(
            &selected_gate_variant,
            &runtime_avoidances(None, Some(&selected_gate_prior))
        )
        .is_some());
    }

    #[test]
    fn alpha_zoo_crowded_root_gene_lowers_reward() {
        let report = sample_report("auto_settlement_alpha_zoo_crowded_candidate");
        let runtime_avoidances = Vec::new();
        let baseline_reward = reward(&report, &runtime_avoidances, None, &Vec::new());

        let zoo = AlphaZooSnapshot {
            version: "alpha_zoo_v1".to_string(),
            target: "full_depth_settlement_executable_pnl".to_string(),
            side: None,
            entries: vec![AlphaZooEntry {
                root_gene: root_gene(&report.expr),
                count: 50,
            }],
        };
        let penalized_reward = reward(&report, &runtime_avoidances, Some(&zoo), &Vec::new());

        assert!(
            penalized_reward < baseline_reward,
            "a root gene crowded across all historical accepted factors should lower reward \
             relative to the same report scored with no Alpha Zoo snapshot"
        );
        assert!(
            alpha_zoo_novelty_penalty(&report.expr, report.target.as_deref(), Some(&zoo)) > 0.0
        );
    }

    #[test]
    fn alpha_zoo_snapshot_for_a_different_target_is_a_no_op() {
        let report = sample_report("auto_settlement_alpha_zoo_cross_target_candidate");
        let runtime_avoidances = Vec::new();
        let baseline_reward = reward(&report, &runtime_avoidances, None, &Vec::new());

        // The snapshot's root gene matches, but its `target` does not match
        // this report's target. A snapshot exported for one search target
        // (e.g. settlement) must never penalize candidates from an unrelated
        // target (e.g. reprice) just because they share a root operator.
        let zoo = AlphaZooSnapshot {
            version: "alpha_zoo_v1".to_string(),
            target: "full_depth_reprice_pnl_10s".to_string(),
            side: None,
            entries: vec![AlphaZooEntry {
                root_gene: root_gene(&report.expr),
                count: 50,
            }],
        };
        let reward_with_mismatched_zoo =
            reward(&report, &runtime_avoidances, Some(&zoo), &Vec::new());

        assert_eq!(
            baseline_reward, reward_with_mismatched_zoo,
            "a snapshot exported for a different target must not affect reward"
        );
        assert_eq!(
            alpha_zoo_novelty_penalty(&report.expr, report.target.as_deref(), Some(&zoo)),
            0.0
        );
        assert_eq!(
            alpha_zoo_novelty_score(&report.expr, report.target.as_deref(), Some(&zoo)),
            1.0
        );
    }

    #[test]
    fn alpha_zoo_absent_is_a_no_op_for_reward() {
        let report = sample_report("auto_settlement_alpha_zoo_no_op_candidate");
        let runtime_avoidances = Vec::new();

        let reward_without_zoo = reward(&report, &runtime_avoidances, None, &Vec::new());

        // An empty snapshot, and no snapshot at all, must be equivalent: absence
        // of Alpha Zoo evidence should never change search behavior.
        let empty_zoo = AlphaZooSnapshot {
            version: "alpha_zoo_v1".to_string(),
            target: "full_depth_settlement_executable_pnl".to_string(),
            side: None,
            entries: Vec::new(),
        };
        let reward_with_empty_zoo =
            reward(&report, &runtime_avoidances, Some(&empty_zoo), &Vec::new());

        assert_eq!(
            reward_without_zoo, reward_with_empty_zoo,
            "omitting the Alpha Zoo snapshot must match an empty snapshot with no matching entries"
        );
        assert_eq!(
            alpha_zoo_novelty_penalty(&report.expr, report.target.as_deref(), None),
            0.0
        );
        assert_eq!(
            alpha_zoo_novelty_score(&report.expr, report.target.as_deref(), None),
            1.0
        );
    }
}
