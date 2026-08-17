use super::DeterministicRng;
use crate::{
    baselines::{
        evaluate_factor_features_from_entries, validate_cex_context_bindings,
        verify_cex_baseline_artifact,
    },
    evaluation::{EngineContext, ResearchRow},
    formula_evaluator::{evaluate_ast, FormulaEvaluator},
    CandidateEvaluation,
};
use alpha_domain::{
    canonical_json_hash, CexBaselineArtifactV1, CexBaselineGateV1, CexBaselineModelKindV1,
    CexEqualAbsoluteWeightPolicyV1, CexFactorBankRevisionV2, CexFactorOrientationV1,
    CexFactorWeightRuleV1, CexResearchContentRefV1, CexResearchHoldoutStateV1, CexResearchMarketV1,
    CexResearchMissionArtifactV1, CexResearchVenueV1, EvaluationCostsV1, EvaluationLabelSpecV1,
    FormulaEvaluatorConfig, SearchBudget, CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION,
    WALK_FORWARD_EVALUATOR_VERSION,
};
use hft_factor_dsl::{validate_live_formula, FactorAst, FactorOperator, FactorTerminal};
use hft_search_kernel::{
    backpropagate_lineage, select_expandable_progressively, validate_tree, UctNode, UctStats,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const CEX_FACTOR_BANK_MCTS_IMPLEMENTATION_VERSION: &str = "cex-factor-bank-subset-mcts-v1";
const CHECKPOINT_SCHEMA_VERSION: &str = "cex-factor-bank-subset-mcts-checkpoint-v1";
const RESULT_SCHEMA_VERSION: &str = "cex-factor-bank-subset-mcts-result-v1";
const SIGNAL_STAGE_SCHEMA_VERSION: &str = "cex-combination-signal-stage-v1";
const SIZING_STAGE_SCHEMA_VERSION: &str = "cex-combination-sizing-stage-v1";
const RISK_STAGE_SCHEMA_VERSION: &str = "cex-combination-risk-stage-v1";
const EXECUTION_STAGE_SCHEMA_VERSION: &str = "cex-combination-execution-stage-v1";
const WALK_FORWARD_EVIDENCE_SCHEMA_VERSION: &str = "cex-combination-walk-forward-evidence-v1";
const COMBINATION_ARTIFACT_SCHEMA_VERSION: &str = "cex-combination-research-artifact-v1";
const UCT_EXPLORATION: f64 = std::f64::consts::SQRT_2;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexFactorIdentityV1 {
    pub factor_id: String,
    pub content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexFactorSubsetV1 {
    pub factors: Vec<CexFactorIdentityV1>,
}

impl CexFactorSubsetV1 {
    pub fn content_hash(&self) -> Result<String, String> {
        canonical_json_hash(self).map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CexFactorSubsetActionV1 {
    Add {
        factor_bank_revision_id: String,
        factor: CexFactorIdentityV1,
    },
    Remove {
        factor_bank_revision_id: String,
        factor: CexFactorIdentityV1,
    },
    Swap {
        factor_bank_revision_id: String,
        remove: CexFactorIdentityV1,
        add: CexFactorIdentityV1,
    },
}

impl CexFactorSubsetActionV1 {
    fn factor_bank_revision_id(&self) -> &str {
        match self {
            Self::Add {
                factor_bank_revision_id,
                ..
            }
            | Self::Remove {
                factor_bank_revision_id,
                ..
            }
            | Self::Swap {
                factor_bank_revision_id,
                ..
            } => factor_bank_revision_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexFactorBankMctsStopReasonV1 {
    Paused,
    CandidateBudgetExhausted,
    ExpansionBudgetExhausted,
    SearchSpaceExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingsV1 {
    implementation_version: String,
    mission_id: String,
    factor_bank_revision_id: String,
    ridge_artifact_id: String,
    cart_artifact_id: String,
    baseline_gate_id: String,
    seed: u64,
    budget: SearchBudget,
    subset_policy: CexResearchContentRefV1,
    weight_policy: CexResearchContentRefV1,
    screening_policy: CexResearchContentRefV1,
    scoring_policy: CexResearchContentRefV1,
    min_subset_size: usize,
    max_subset_size: usize,
}

impl BindingsV1 {
    fn validate(&self) -> Result<(), String> {
        if self.implementation_version != CEX_FACTOR_BANK_MCTS_IMPLEMENTATION_VERSION
            || self.mission_id.trim().is_empty()
            || self.factor_bank_revision_id.trim().is_empty()
            || self.ridge_artifact_id.trim().is_empty()
            || self.cart_artifact_id.trim().is_empty()
            || self.ridge_artifact_id == self.cart_artifact_id
            || self.baseline_gate_id.trim().is_empty()
            || self.min_subset_size != 1
            || self.max_subset_size < self.min_subset_size
            || self.budget.max_expansions == 0
            || self.budget.max_tokens != 0
            || self.budget.max_seconds != 0
        {
            return Err("Factor-Bank MCTS bindings are invalid".to_string());
        }
        self.budget.validate().map_err(|error| error.to_string())?;
        for policy in [
            &self.subset_policy,
            &self.weight_policy,
            &self.screening_policy,
            &self.scoring_policy,
        ] {
            policy.validate().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeV1 {
    state: CexFactorSubsetV1,
    parent: Option<usize>,
    children: Vec<usize>,
    remaining_actions: Vec<CexFactorSubsetActionV1>,
    subtree_has_expansion: bool,
    action: Option<CexFactorSubsetActionV1>,
    depth: usize,
    visits: u64,
    total_reward: f64,
    best_reward: Option<f64>,
    evaluation: Option<CandidateEvaluation>,
}

impl UctNode for NodeV1 {
    fn parent(&self) -> Option<usize> {
        self.parent
    }

    fn children(&self) -> &[usize] {
        &self.children
    }

    fn is_expandable(&self) -> bool {
        !self.remaining_actions.is_empty()
    }

    fn depth(&self) -> usize {
        self.depth
    }

    fn stats(&self) -> Result<UctStats, hft_search_kernel::UctError> {
        UctStats::from_parts(self.visits, self.total_reward, self.best_reward)
    }

    fn replace_stats(&mut self, stats: UctStats) {
        self.visits = stats.visits();
        self.total_reward = stats.total_reward();
        self.best_reward = stats.best_reward();
    }
    fn subtree_is_expandable(&self) -> bool {
        self.subtree_has_expansion
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum TraceOutcomeV1 {
    Evaluated {
        node_id: usize,
        reward: f64,
        evaluation_sha256: String,
    },
    Duplicate {
        state_sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexFactorBankMctsTraceStepV1 {
    pub step: u64,
    pub parent_node_id: usize,
    pub action: CexFactorSubsetActionV1,
    pub resulting_state: CexFactorSubsetV1,
    outcome: TraceOutcomeV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexFactorBankMctsCheckpointV1 {
    schema_version: String,
    bindings: BindingsV1,
    rng: DeterministicRng,
    nodes: Vec<NodeV1>,
    seen_state_sha256: BTreeSet<String>,
    trace: Vec<CexFactorBankMctsTraceStepV1>,
    expansions_used: u64,
    candidates_evaluated: usize,
    selected_node_id: Option<usize>,
    terminal_reason: Option<CexFactorBankMctsStopReasonV1>,
}

impl CexFactorBankMctsCheckpointV1 {
    pub fn content_hash(&self) -> Result<String, String> {
        let value = serde_json::to_value(self).map_err(|error| error.to_string())?;
        canonical_json_hash(&value).map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexFactorBankMctsSelectionV1 {
    pub subset: CexFactorSubsetV1,
    pub normalized_equal_abs_weight: f64,
    pub evaluation: CandidateEvaluation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexFactorBankMctsResultV1 {
    pub schema_version: String,
    pub implementation_version: String,
    pub mission_id: String,
    pub factor_bank_revision_id: String,
    pub baseline_gate_id: String,
    pub terminal_reason: CexFactorBankMctsStopReasonV1,
    pub expansions_used: u64,
    pub candidates_evaluated: usize,
    pub selected: Option<CexFactorBankMctsSelectionV1>,
    pub checkpoint_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexCombinationNormalizationV1 {
    NoneRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexSignalThresholdPolicyV1 {
    Zero,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexSizingRuleV1 {
    ZeroWithinMachineEpsilonElseSign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexResearchOrderSemanticsV1 {
    ValidationBucketTargetPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexResearchEventModalityV1 {
    BucketedPointInTimeL2Features,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexCombinationEvaluationKindV1 {
    SelectedSubset,
    RidgeBaseline,
    CartBaseline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexSignalFactorV1 {
    pub factor: CexFactorIdentityV1,
    pub orientation: CexFactorOrientationV1,
    pub normalized_absolute_weight: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexSignalStageV1 {
    pub schema_version: String,
    pub content_sha256: String,
    pub parent: CexResearchContentRefV1,
    pub subset_policy: CexResearchContentRefV1,
    pub weight_policy: CexResearchContentRefV1,
    pub factors: Vec<CexSignalFactorV1>,
    pub combination_rule: CexFactorWeightRuleV1,
    pub normalization: CexCombinationNormalizationV1,
    pub threshold_policy: CexSignalThresholdPolicyV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexSizingStageV1 {
    pub schema_version: String,
    pub content_sha256: String,
    pub parent: CexResearchContentRefV1,
    pub rule: CexSizingRuleV1,
    pub zero_epsilon: f64,
    pub min_position: f64,
    pub max_position: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexRiskStageV1 {
    pub schema_version: String,
    pub content_sha256: String,
    pub parent: CexResearchContentRefV1,
    pub evaluator_policy: CexResearchContentRefV1,
    pub immutable: bool,
    pub max_abs_position: f64,
    pub position_notional_usd: f64,
    pub max_drawdown: f64,
    pub capacity_depth_levels: usize,
    pub max_book_depth_fraction: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexExecutionStageV1 {
    pub schema_version: String,
    pub content_sha256: String,
    pub parent: CexResearchContentRefV1,
    pub evaluation_policy: CexResearchContentRefV1,
    pub venue: CexResearchVenueV1,
    pub market: CexResearchMarketV1,
    pub symbol: String,
    pub order_semantics: CexResearchOrderSemanticsV1,
    pub event_modality: CexResearchEventModalityV1,
    pub costs: EvaluationCostsV1,
    pub horizon: EvaluationLabelSpecV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexCombinationEvaluationEvidenceV1 {
    pub kind: CexCombinationEvaluationKindV1,
    pub source_artifact: CexResearchContentRefV1,
    pub evaluation_sha256: String,
    pub evaluation: CandidateEvaluation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexCombinationWalkForwardEvidenceV1 {
    pub schema_version: String,
    pub content_sha256: String,
    pub research_dataset: CexResearchContentRefV1,
    pub walk_forward_partition: CexResearchContentRefV1,
    pub evaluation_protocol: CexResearchContentRefV1,
    pub holdout_id: String,
    pub holdout_state: CexResearchHoldoutStateV1,
    pub selected: CexCombinationEvaluationEvidenceV1,
    pub ridge: CexCombinationEvaluationEvidenceV1,
    pub cart: CexCombinationEvaluationEvidenceV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexCombinationResearchArtifactV1 {
    pub schema_version: String,
    pub artifact_id: String,
    pub strategy_id: String,
    pub mission_id: String,
    pub subset_result: CexResearchContentRefV1,
    pub subset_checkpoint: CexResearchContentRefV1,
    pub signal: CexSignalStageV1,
    pub sizing: CexSizingStageV1,
    pub risk: CexRiskStageV1,
    pub execution: CexExecutionStageV1,
    pub walk_forward_evidence: CexCombinationWalkForwardEvidenceV1,
    pub deployment_authority: bool,
    pub order_submission_authority: bool,
}

impl CexSignalStageV1 {
    fn new(
        parent: CexResearchContentRefV1,
        subset_policy: CexResearchContentRefV1,
        weight_policy: CexResearchContentRefV1,
        factors: Vec<CexSignalFactorV1>,
    ) -> Result<Self, String> {
        let mut stage = Self {
            schema_version: SIGNAL_STAGE_SCHEMA_VERSION.to_string(),
            content_sha256: String::new(),
            parent,
            subset_policy,
            weight_policy,
            factors,
            combination_rule: CexFactorWeightRuleV1::OrientedEqualAbsoluteSumToOne,
            normalization: CexCombinationNormalizationV1::NoneRequired,
            threshold_policy: CexSignalThresholdPolicyV1::Zero,
        };
        stage.content_sha256 = stage.expected_content_sha256()?;
        stage.validate()?;
        Ok(stage)
    }

    fn validate(&self) -> Result<(), String> {
        self.parent.validate().map_err(|error| error.to_string())?;
        self.subset_policy
            .validate()
            .map_err(|error| error.to_string())?;
        self.weight_policy
            .validate()
            .map_err(|error| error.to_string())?;
        let expected_weight = 1.0 / self.factors.len().max(1) as f64;
        if self.schema_version != SIGNAL_STAGE_SCHEMA_VERSION
            || self.content_sha256 != self.expected_content_sha256()?
            || self.factors.is_empty()
            || self
                .factors
                .windows(2)
                .any(|pair| pair[0].factor.factor_id >= pair[1].factor.factor_id)
            || self.factors.iter().any(|factor| {
                factor.factor.factor_id.trim().is_empty()
                    || !valid_sha256(&factor.factor.content_sha256)
                    || factor.normalized_absolute_weight.to_bits() != expected_weight.to_bits()
            })
            || self.combination_rule != CexFactorWeightRuleV1::OrientedEqualAbsoluteSumToOne
            || self.normalization != CexCombinationNormalizationV1::NoneRequired
            || self.threshold_policy != CexSignalThresholdPolicyV1::Zero
        {
            return Err("CEX combination Signal stage is invalid".to_string());
        }
        Ok(())
    }

    fn reference(&self) -> Result<CexResearchContentRefV1, String> {
        self.validate()?;
        Ok(stage_reference(
            "cex-combination-signal-stage",
            &self.content_sha256,
        ))
    }

    fn expected_content_sha256(&self) -> Result<String, String> {
        let mut semantic = self.clone();
        semantic.content_sha256.clear();
        canonical_json_hash(&semantic).map_err(|error| error.to_string())
    }
}

impl CexSizingStageV1 {
    fn new(parent: CexResearchContentRefV1) -> Result<Self, String> {
        let mut stage = Self {
            schema_version: SIZING_STAGE_SCHEMA_VERSION.to_string(),
            content_sha256: String::new(),
            parent,
            rule: CexSizingRuleV1::ZeroWithinMachineEpsilonElseSign,
            zero_epsilon: f64::EPSILON,
            min_position: -1.0,
            max_position: 1.0,
        };
        stage.content_sha256 = stage.expected_content_sha256()?;
        stage.validate()?;
        Ok(stage)
    }

    fn validate(&self) -> Result<(), String> {
        self.parent.validate().map_err(|error| error.to_string())?;
        if self.schema_version != SIZING_STAGE_SCHEMA_VERSION
            || self.content_sha256 != self.expected_content_sha256()?
            || self.rule != CexSizingRuleV1::ZeroWithinMachineEpsilonElseSign
            || self.zero_epsilon.to_bits() != f64::EPSILON.to_bits()
            || self.min_position.to_bits() != (-1.0_f64).to_bits()
            || self.max_position.to_bits() != 1.0_f64.to_bits()
        {
            return Err("CEX combination Sizing stage is invalid".to_string());
        }
        Ok(())
    }

    fn reference(&self) -> Result<CexResearchContentRefV1, String> {
        self.validate()?;
        Ok(stage_reference(
            "cex-combination-sizing-stage",
            &self.content_sha256,
        ))
    }

    fn expected_content_sha256(&self) -> Result<String, String> {
        let mut semantic = self.clone();
        semantic.content_sha256.clear();
        canonical_json_hash(&semantic).map_err(|error| error.to_string())
    }
}

impl CexRiskStageV1 {
    #[allow(clippy::too_many_arguments)]
    fn new(
        parent: CexResearchContentRefV1,
        evaluator_policy: CexResearchContentRefV1,
        position_notional_usd: f64,
        max_drawdown: f64,
        capacity_depth_levels: usize,
        max_book_depth_fraction: f64,
    ) -> Result<Self, String> {
        let mut stage = Self {
            schema_version: RISK_STAGE_SCHEMA_VERSION.to_string(),
            content_sha256: String::new(),
            parent,
            evaluator_policy,
            immutable: true,
            max_abs_position: 1.0,
            position_notional_usd,
            max_drawdown,
            capacity_depth_levels,
            max_book_depth_fraction,
        };
        stage.content_sha256 = stage.expected_content_sha256()?;
        stage.validate()?;
        Ok(stage)
    }

    fn validate(&self) -> Result<(), String> {
        self.parent.validate().map_err(|error| error.to_string())?;
        self.evaluator_policy
            .validate()
            .map_err(|error| error.to_string())?;
        let capacity_disabled = self.position_notional_usd == 0.0
            && self.capacity_depth_levels == 0
            && self.max_book_depth_fraction == 0.0;
        let capacity_enabled = self.position_notional_usd.is_finite()
            && self.position_notional_usd > 0.0
            && self.capacity_depth_levels > 0
            && self.max_book_depth_fraction.is_finite()
            && self.max_book_depth_fraction > 0.0
            && self.max_book_depth_fraction <= 1.0;
        if self.schema_version != RISK_STAGE_SCHEMA_VERSION
            || self.content_sha256 != self.expected_content_sha256()?
            || !self.immutable
            || self.max_abs_position.to_bits() != 1.0_f64.to_bits()
            || !self.max_drawdown.is_finite()
            || self.max_drawdown <= 0.0
            || self.max_drawdown > 1.0
            || !(capacity_disabled || capacity_enabled)
        {
            return Err("CEX combination Risk stage is invalid".to_string());
        }
        Ok(())
    }

    fn reference(&self) -> Result<CexResearchContentRefV1, String> {
        self.validate()?;
        Ok(stage_reference(
            "cex-combination-risk-stage",
            &self.content_sha256,
        ))
    }

    fn expected_content_sha256(&self) -> Result<String, String> {
        let mut semantic = self.clone();
        semantic.content_sha256.clear();
        canonical_json_hash(&semantic).map_err(|error| error.to_string())
    }
}

impl CexExecutionStageV1 {
    #[allow(clippy::too_many_arguments)]
    fn new(
        parent: CexResearchContentRefV1,
        evaluation_policy: CexResearchContentRefV1,
        venue: CexResearchVenueV1,
        market: CexResearchMarketV1,
        symbol: String,
        costs: EvaluationCostsV1,
        horizon: EvaluationLabelSpecV1,
    ) -> Result<Self, String> {
        let mut stage = Self {
            schema_version: EXECUTION_STAGE_SCHEMA_VERSION.to_string(),
            content_sha256: String::new(),
            parent,
            evaluation_policy,
            venue,
            market,
            symbol,
            order_semantics: CexResearchOrderSemanticsV1::ValidationBucketTargetPosition,
            event_modality: CexResearchEventModalityV1::BucketedPointInTimeL2Features,
            costs,
            horizon,
        };
        stage.content_sha256 = stage.expected_content_sha256()?;
        stage.validate()?;
        Ok(stage)
    }

    fn validate(&self) -> Result<(), String> {
        self.parent.validate().map_err(|error| error.to_string())?;
        self.evaluation_policy
            .validate()
            .map_err(|error| error.to_string())?;
        if self.schema_version != EXECUTION_STAGE_SCHEMA_VERSION
            || self.content_sha256 != self.expected_content_sha256()?
            || self.symbol.trim().is_empty()
            || self.symbol != self.symbol.to_ascii_uppercase()
            || self.order_semantics != CexResearchOrderSemanticsV1::ValidationBucketTargetPosition
            || self.event_modality != CexResearchEventModalityV1::BucketedPointInTimeL2Features
            || self.horizon.horizon_buckets == 0
            || self.horizon.observation_frequency_millis == 0
        {
            return Err("CEX combination Execution stage is invalid".to_string());
        }
        Ok(())
    }

    fn reference(&self) -> Result<CexResearchContentRefV1, String> {
        self.validate()?;
        Ok(stage_reference(
            "cex-combination-execution-stage",
            &self.content_sha256,
        ))
    }

    fn expected_content_sha256(&self) -> Result<String, String> {
        let mut semantic = self.clone();
        semantic.content_sha256.clear();
        canonical_json_hash(&semantic).map_err(|error| error.to_string())
    }
}

impl CexCombinationEvaluationEvidenceV1 {
    fn new(
        kind: CexCombinationEvaluationKindV1,
        source_artifact: CexResearchContentRefV1,
        evaluation: CandidateEvaluation,
    ) -> Result<Self, String> {
        let evidence = Self {
            kind,
            source_artifact,
            evaluation_sha256: canonical_json_hash(&evaluation)
                .map_err(|error| error.to_string())?,
            evaluation,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    fn validate(&self) -> Result<(), String> {
        self.source_artifact
            .validate()
            .map_err(|error| error.to_string())?;
        self.evaluation
            .validate()
            .map_err(|error| error.to_string())?;
        if self.evaluation_sha256
            != canonical_json_hash(&self.evaluation).map_err(|error| error.to_string())?
        {
            return Err("CEX combination evaluation identity drifted".to_string());
        }
        Ok(())
    }
}

impl CexCombinationWalkForwardEvidenceV1 {
    #[allow(clippy::too_many_arguments)]
    fn new(
        research_dataset: CexResearchContentRefV1,
        walk_forward_partition: CexResearchContentRefV1,
        evaluation_protocol: CexResearchContentRefV1,
        holdout_id: String,
        holdout_state: CexResearchHoldoutStateV1,
        selected: CexCombinationEvaluationEvidenceV1,
        ridge: CexCombinationEvaluationEvidenceV1,
        cart: CexCombinationEvaluationEvidenceV1,
    ) -> Result<Self, String> {
        let mut evidence = Self {
            schema_version: WALK_FORWARD_EVIDENCE_SCHEMA_VERSION.to_string(),
            content_sha256: String::new(),
            research_dataset,
            walk_forward_partition,
            evaluation_protocol,
            holdout_id,
            holdout_state,
            selected,
            ridge,
            cart,
        };
        evidence.content_sha256 = evidence.expected_content_sha256()?;
        evidence.validate()?;
        Ok(evidence)
    }

    fn validate(&self) -> Result<(), String> {
        for reference in [
            &self.research_dataset,
            &self.walk_forward_partition,
            &self.evaluation_protocol,
        ] {
            reference.validate().map_err(|error| error.to_string())?;
        }
        self.selected.validate()?;
        self.ridge.validate()?;
        self.cart.validate()?;
        let evaluations = [
            &self.selected.evaluation,
            &self.ridge.evaluation,
            &self.cart.evaluation,
        ];
        if self.schema_version != WALK_FORWARD_EVIDENCE_SCHEMA_VERSION
            || self.content_sha256 != self.expected_content_sha256()?
            || self.holdout_id.trim().is_empty()
            || self.holdout_state != CexResearchHoldoutStateV1::Unopened
            || self.selected.kind != CexCombinationEvaluationKindV1::SelectedSubset
            || self.ridge.kind != CexCombinationEvaluationKindV1::RidgeBaseline
            || self.cart.kind != CexCombinationEvaluationKindV1::CartBaseline
            || self.selected.evaluation.evaluator_version != WALK_FORWARD_EVALUATOR_VERSION
            || self.ridge.evaluation.evaluator_version
                != CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION
            || self.cart.evaluation.evaluator_version != CEX_BASELINE_WALK_FORWARD_EVALUATOR_VERSION
            || evaluations.iter().any(|evaluation| {
                !evaluation.passed
                    || evaluation.protocol_binding().map_or(true, |(_, hash)| {
                        hash != self.evaluation_protocol.content_sha256
                    })
            })
        {
            return Err("CEX combination walk-forward evidence is invalid".to_string());
        }
        Ok(())
    }

    fn expected_content_sha256(&self) -> Result<String, String> {
        let mut semantic = self.clone();
        semantic.content_sha256.clear();
        canonical_json_hash(&semantic).map_err(|error| error.to_string())
    }
}

impl CexCombinationResearchArtifactV1 {
    fn from_validated_search(
        mission: &CexResearchMissionArtifactV1,
        factor_bank: &CexFactorBankRevisionV2,
        ridge: &CexBaselineArtifactV1,
        cart: &CexBaselineArtifactV1,
        gate: &CexBaselineGateV1,
        result: &CexFactorBankMctsResultV1,
        checkpoint: &CexFactorBankMctsCheckpointV1,
    ) -> Result<Self, String> {
        validate_combination_sources(mission, factor_bank, ridge, cart, gate, result)?;
        let checkpoint_sha256 = checkpoint.content_hash()?;
        if checkpoint_sha256 != result.checkpoint_sha256 {
            return Err("CEX combination checkpoint identity drifted".to_string());
        }
        let selection = result.selected.as_ref().ok_or_else(|| {
            "CEX combination strategy requires a passing selected subset".to_string()
        })?;
        let factors = selection
            .subset
            .factors
            .iter()
            .map(|factor| {
                let entry = factor_bank
                    .entries
                    .iter()
                    .find(|entry| entry.factor_id == factor.factor_id)
                    .ok_or_else(|| "CEX combination selected an unknown Factor ID".to_string())?;
                Ok(CexSignalFactorV1 {
                    factor: factor.clone(),
                    orientation: entry.orientation,
                    normalized_absolute_weight: selection.normalized_equal_abs_weight,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let factor_bank_ref = artifact_reference(&factor_bank.revision_id, factor_bank)?;
        let signal = CexSignalStageV1::new(
            factor_bank_ref,
            mission.spec.policies.subset_search.clone(),
            mission.spec.policies.weight.clone(),
            factors,
        )?;
        let sizing = CexSizingStageV1::new(signal.reference()?)?;
        let evaluator_config = selection
            .evaluation
            .formula_config()
            .map_err(|error| error.to_string())?;
        let protocol = selection
            .evaluation
            .protocol_binding()
            .map_err(|error| error.to_string())?
            .0
            .clone();
        let risk = CexRiskStageV1::new(
            sizing.reference()?,
            mission.spec.policies.screening.clone(),
            protocol.costs.position_notional_usd,
            evaluator_config.max_drawdown,
            protocol.costs.capacity_depth_levels,
            protocol.costs.max_book_depth_fraction,
        )?;
        let execution = CexExecutionStageV1::new(
            risk.reference()?,
            mission.spec.policies.evaluation.clone(),
            mission.spec.instrument.venue.clone(),
            mission.spec.instrument.market.clone(),
            mission.spec.instrument.symbol.clone(),
            protocol.costs.clone(),
            protocol.labels.clone(),
        )?;
        let result_hash = canonical_json_hash(result).map_err(|error| error.to_string())?;
        let subset_result = CexResearchContentRefV1 {
            id: format!("cex-factor-bank-subset-mcts-result-{result_hash}"),
            content_sha256: result_hash,
        };
        let subset_checkpoint = CexResearchContentRefV1 {
            id: format!("cex-factor-bank-subset-mcts-checkpoint-{checkpoint_sha256}"),
            content_sha256: checkpoint_sha256,
        };
        let walk_forward_evidence = CexCombinationWalkForwardEvidenceV1::new(
            factor_bank.research_dataset.clone(),
            factor_bank.walk_forward_partition.clone(),
            mission.spec.policies.evaluation.clone(),
            mission.spec.holdout.holdout_id.clone(),
            mission.spec.holdout.state,
            CexCombinationEvaluationEvidenceV1::new(
                CexCombinationEvaluationKindV1::SelectedSubset,
                subset_result.clone(),
                selection.evaluation.clone(),
            )?,
            CexCombinationEvaluationEvidenceV1::new(
                CexCombinationEvaluationKindV1::RidgeBaseline,
                artifact_reference(&ridge.artifact_id, ridge)?,
                ridge.evaluation.clone(),
            )?,
            CexCombinationEvaluationEvidenceV1::new(
                CexCombinationEvaluationKindV1::CartBaseline,
                artifact_reference(&cart.artifact_id, cart)?,
                cart.evaluation.clone(),
            )?,
        )?;
        let mut artifact = Self {
            schema_version: COMBINATION_ARTIFACT_SCHEMA_VERSION.to_string(),
            artifact_id: String::new(),
            strategy_id: String::new(),
            mission_id: result.mission_id.clone(),
            subset_result,
            subset_checkpoint,
            signal,
            sizing,
            risk,
            execution,
            walk_forward_evidence,
            deployment_authority: false,
            order_submission_authority: false,
        };
        artifact.strategy_id = artifact.expected_strategy_id()?;
        artifact.artifact_id = artifact.expected_artifact_id()?;
        artifact.validate()?;
        Ok(artifact)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.subset_result
            .validate()
            .map_err(|error| error.to_string())?;
        self.subset_checkpoint
            .validate()
            .map_err(|error| error.to_string())?;
        self.signal.validate()?;
        self.sizing.validate()?;
        self.risk.validate()?;
        self.execution.validate()?;
        self.walk_forward_evidence.validate()?;
        let protocol = self
            .walk_forward_evidence
            .selected
            .evaluation
            .protocol_binding()
            .map_err(|error| error.to_string())?
            .0;
        let evaluator_config = self
            .walk_forward_evidence
            .selected
            .evaluation
            .formula_config()
            .map_err(|error| error.to_string())?;
        if self.schema_version != COMBINATION_ARTIFACT_SCHEMA_VERSION
            || self.artifact_id != self.expected_artifact_id()?
            || self.strategy_id != self.expected_strategy_id()?
            || self.mission_id.trim().is_empty()
            || self.deployment_authority
            || self.order_submission_authority
            || self.subset_result.id
                != format!(
                    "cex-factor-bank-subset-mcts-result-{}",
                    self.subset_result.content_sha256
                )
            || self.subset_checkpoint.id
                != format!(
                    "cex-factor-bank-subset-mcts-checkpoint-{}",
                    self.subset_checkpoint.content_sha256
                )
            || self.sizing.parent != self.signal.reference()?
            || self.risk.parent != self.sizing.reference()?
            || self.execution.parent != self.risk.reference()?
            || self.subset_result != self.walk_forward_evidence.selected.source_artifact
            || self.execution.evaluation_policy != self.walk_forward_evidence.evaluation_protocol
            || self.execution.costs != protocol.costs
            || self.execution.horizon != protocol.labels
            || self.risk.evaluator_policy.content_sha256
                != canonical_json_hash(&evaluator_config).map_err(|error| error.to_string())?
            || self.risk.max_drawdown.to_bits() != evaluator_config.max_drawdown.to_bits()
            || self.risk.position_notional_usd.to_bits()
                != protocol.costs.position_notional_usd.to_bits()
            || self.risk.capacity_depth_levels != protocol.costs.capacity_depth_levels
            || self.risk.max_book_depth_fraction.to_bits()
                != protocol.costs.max_book_depth_fraction.to_bits()
        {
            return Err("CEX combination research artifact is invalid".to_string());
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn validate_binding(
        &self,
        mission: &CexResearchMissionArtifactV1,
        factor_bank: &CexFactorBankRevisionV2,
        ridge: &CexBaselineArtifactV1,
        cart: &CexBaselineArtifactV1,
        gate: &CexBaselineGateV1,
        context: &EngineContext<'_>,
        checkpoint: &CexFactorBankMctsCheckpointV1,
        result: &CexFactorBankMctsResultV1,
    ) -> Result<(), String> {
        self.validate()?;
        let restored = CexFactorBankMcts::restore_json(
            mission,
            factor_bank,
            ridge,
            cart,
            gate,
            context,
            serde_json::to_value(checkpoint).map_err(|error| error.to_string())?,
        )?;
        if restored.result()? != *result {
            return Err(
                "CEX combination subset result is not derived from its checkpoint".to_string(),
            );
        }
        let expected = restored
            .combination_artifact(mission, ridge, cart, gate)?
            .ok_or_else(|| "CEX combination checkpoint has no passing selection".to_string())?;
        if self != &expected {
            return Err("CEX combination research artifact binding drifted".to_string());
        }
        Ok(())
    }

    pub fn target_positions(
        &self,
        factor_bank: &CexFactorBankRevisionV2,
        rows: &[ResearchRow],
    ) -> Result<Vec<f64>, String> {
        let formula = self.executable_formula(factor_bank)?;
        let combined = evaluate_ast(&formula, rows)?;
        if combined.iter().any(|value| !value.is_finite()) {
            return Err("CEX replay strategy produced a non-finite signal".to_string());
        }
        Ok(combined
            .into_iter()
            .map(|signal| {
                if signal.abs() <= self.sizing.zero_epsilon {
                    0.0
                } else {
                    signal
                        .signum()
                        .clamp(self.sizing.min_position, self.sizing.max_position)
                }
            })
            .collect())
    }

    pub fn executable_formula(
        &self,
        factor_bank: &CexFactorBankRevisionV2,
    ) -> Result<FactorAst, String> {
        self.validate()?;
        factor_bank.validate().map_err(|error| error.to_string())?;
        if self.signal.parent != artifact_reference(&factor_bank.revision_id, factor_bank)? {
            return Err("CEX replay Factor Bank identity drifted".to_string());
        }
        let mut formula = FactorAst::Terminal(FactorTerminal::Constant("0".to_string()));
        for selected in &self.signal.factors {
            let entry = factor_bank
                .entries
                .iter()
                .find(|entry| entry.factor_id == selected.factor.factor_id)
                .ok_or_else(|| "CEX replay selected an unknown Factor ID".to_string())?;
            if selected.factor.content_sha256
                != canonical_json_hash(entry).map_err(|error| error.to_string())?
                || selected.orientation != entry.orientation
            {
                return Err("CEX replay factor identity drifted".to_string());
            }
            let orientation = match selected.orientation {
                CexFactorOrientationV1::Positive => 1.0,
                CexFactorOrientationV1::Negative => -1.0,
            };
            let weighted = FactorAst::call(
                FactorOperator::Mul,
                vec![
                    FactorAst::Terminal(FactorTerminal::Constant(
                        (orientation * selected.normalized_absolute_weight).to_string(),
                    )),
                    entry.canonical_ast.clone(),
                ],
            )
            .map_err(|error| error.to_string())?;
            formula = FactorAst::call(FactorOperator::Add, vec![formula, weighted])
                .map_err(|error| error.to_string())?;
        }
        validate_live_formula(&formula).map_err(|error| error.to_string())?;
        Ok(formula)
    }

    fn expected_strategy_id(&self) -> Result<String, String> {
        let identity = serde_json::json!({
            "mission_id": self.mission_id,
            "signal": self.signal.reference()?,
            "sizing": self.sizing.reference()?,
            "risk": self.risk.reference()?,
            "execution": self.execution.reference()?,
        });
        Ok(format!(
            "cex-combination-strategy-{}",
            canonical_json_hash(&identity).map_err(|error| error.to_string())?
        ))
    }

    fn expected_artifact_id(&self) -> Result<String, String> {
        let mut semantic = self.clone();
        semantic.artifact_id.clear();
        Ok(format!(
            "cex-combination-research-artifact-{}",
            canonical_json_hash(&semantic).map_err(|error| error.to_string())?
        ))
    }
}

fn stage_reference(prefix: &str, content_sha256: &str) -> CexResearchContentRefV1 {
    CexResearchContentRefV1 {
        id: format!("{prefix}-{content_sha256}"),
        content_sha256: content_sha256.to_string(),
    }
}

fn artifact_reference(
    id: &str,
    artifact: &impl Serialize,
) -> Result<CexResearchContentRefV1, String> {
    Ok(CexResearchContentRefV1 {
        id: id.to_string(),
        content_sha256: canonical_json_hash(artifact).map_err(|error| error.to_string())?,
    })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub struct CexFactorBankMcts {
    factor_bank: CexFactorBankRevisionV2,
    evaluator: FormulaEvaluator,
    checkpoint: CexFactorBankMctsCheckpointV1,
}

impl CexFactorBankMcts {
    pub fn new(
        mission: &CexResearchMissionArtifactV1,
        factor_bank: &CexFactorBankRevisionV2,
        ridge: &CexBaselineArtifactV1,
        cart: &CexBaselineArtifactV1,
        gate: &CexBaselineGateV1,
        context: &EngineContext<'_>,
    ) -> Result<Self, String> {
        validate_start(mission, factor_bank, ridge, cart, gate, context)?;
        let mission_id = mission.semantic_id().map_err(|error| error.to_string())?;
        let evaluator_config = FormulaEvaluatorConfig::for_trials(
            mission
                .spec
                .search
                .planned_gp_and_subset_trials()
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if canonical_json_hash(&evaluator_config).map_err(|error| error.to_string())?
            != mission.spec.policies.screening.content_sha256
        {
            return Err("Factor-Bank MCTS scoring policy drifted".to_string());
        }
        let identities = factor_identities(factor_bank)?;
        let bindings = BindingsV1 {
            implementation_version: CEX_FACTOR_BANK_MCTS_IMPLEMENTATION_VERSION.to_string(),
            mission_id,
            factor_bank_revision_id: factor_bank.revision_id.clone(),
            ridge_artifact_id: ridge.artifact_id.clone(),
            cart_artifact_id: cart.artifact_id.clone(),
            baseline_gate_id: gate.gate_id.clone(),
            seed: mission.spec.search.seed,
            budget: mission.spec.search.budget.clone(),
            subset_policy: mission.spec.policies.subset_search.clone(),
            weight_policy: mission.spec.policies.weight.clone(),
            screening_policy: mission.spec.policies.screening.clone(),
            scoring_policy: mission.spec.policies.evaluation.clone(),
            min_subset_size: 1,
            max_subset_size: identities.len(),
        };
        bindings.validate()?;
        let root = CexFactorSubsetV1 { factors: vec![] };
        let root_hash = root.content_hash()?;
        let root_actions = legal_actions(&root, factor_bank, bindings.max_subset_size)?;
        let checkpoint = CexFactorBankMctsCheckpointV1 {
            schema_version: CHECKPOINT_SCHEMA_VERSION.to_string(),
            rng: DeterministicRng::new(bindings.seed),
            nodes: vec![NodeV1 {
                state: root.clone(),
                parent: None,
                children: vec![],
                subtree_has_expansion: !root_actions.is_empty(),
                remaining_actions: root_actions,
                action: None,
                depth: 0,
                visits: 0,
                total_reward: 0.0,
                best_reward: None,
                evaluation: None,
            }],
            seen_state_sha256: BTreeSet::from([root_hash]),
            trace: vec![],
            expansions_used: 0,
            candidates_evaluated: 0,
            selected_node_id: None,
            terminal_reason: None,
            bindings,
        };
        let search = Self {
            factor_bank: factor_bank.clone(),
            evaluator: FormulaEvaluator::new(evaluator_config)?,
            checkpoint,
        };
        search.validate_checkpoint()?;
        Ok(search)
    }

    pub fn restore_json(
        mission: &CexResearchMissionArtifactV1,
        factor_bank: &CexFactorBankRevisionV2,
        ridge: &CexBaselineArtifactV1,
        cart: &CexBaselineArtifactV1,
        gate: &CexBaselineGateV1,
        context: &EngineContext<'_>,
        value: serde_json::Value,
    ) -> Result<Self, String> {
        if value.get("kind").is_some() && value.get("version").is_some() {
            let version = value
                .get("version")
                .and_then(serde_json::Value::as_u64)
                .map_or_else(|| "unknown".to_string(), |value| value.to_string());
            return Err(format!(
                "legacy Formula-MCTS checkpoint version {version} cannot resume as {CEX_FACTOR_BANK_MCTS_IMPLEMENTATION_VERSION}"
            ));
        }
        let checkpoint: CexFactorBankMctsCheckpointV1 = serde_json::from_value(value)
            .map_err(|error| format!("invalid Factor-Bank MCTS checkpoint: {error}"))?;
        let mut restored = Self::new(mission, factor_bank, ridge, cart, gate, context)?;
        if checkpoint.schema_version != CHECKPOINT_SCHEMA_VERSION {
            return Err(format!(
                "Factor-Bank MCTS checkpoint version {} is incompatible with {CHECKPOINT_SCHEMA_VERSION}",
                checkpoint.schema_version
            ));
        }
        if checkpoint.bindings != restored.checkpoint.bindings {
            return Err("Factor-Bank MCTS checkpoint bindings drifted".to_string());
        }
        restored.checkpoint = checkpoint.clone();
        restored.validate_checkpoint()?;

        let mut replayed = Self::new(mission, factor_bank, ridge, cart, gate, context)?;
        if checkpoint.expansions_used > 0
            && replayed.run(context, Some(checkpoint.expansions_used))?
                != CexFactorBankMctsStopReasonV1::Paused
        {
            return Err("Factor-Bank MCTS restored trace extends past termination".to_string());
        }
        if checkpoint.terminal_reason.is_some() {
            if replayed.expected_terminal_reason()?.is_none() {
                return Err("Factor-Bank MCTS restored terminal state drifted".to_string());
            }
            replayed.run(context, None)?;
        }
        if replayed.checkpoint != checkpoint {
            return Err("Factor-Bank MCTS restored replay drifted".to_string());
        }
        Ok(replayed)
    }

    pub fn run(
        &mut self,
        context: &EngineContext<'_>,
        max_new_transitions: Option<u64>,
    ) -> Result<CexFactorBankMctsStopReasonV1, String> {
        self.validate_checkpoint()?;
        validate_cex_context_bindings(
            context,
            &self.factor_bank,
            &self.checkpoint.bindings.scoring_policy,
        )?;
        if let Some(reason) = self.checkpoint.terminal_reason {
            return Ok(reason);
        }
        let transition_limit = max_new_transitions.unwrap_or(u64::MAX);
        if transition_limit == 0 {
            return Err("Factor-Bank MCTS transition limit must be positive".to_string());
        }
        let mut new_transitions = 0_u64;
        loop {
            if new_transitions >= transition_limit {
                self.validate_checkpoint()?;
                return Ok(CexFactorBankMctsStopReasonV1::Paused);
            }
            if let Some(reason) = self.budget_terminal_reason() {
                self.checkpoint.terminal_reason = Some(reason);
                self.validate_checkpoint()?;
                return Ok(reason);
            }
            let Some(parent_id) =
                select_expandable_progressively(&self.checkpoint.nodes, 0, UCT_EXPLORATION)
                    .map_err(|error| error.to_string())?
            else {
                let reason = CexFactorBankMctsStopReasonV1::SearchSpaceExhausted;
                self.checkpoint.terminal_reason = Some(reason);
                self.validate_checkpoint()?;
                return Ok(reason);
            };
            let node = &self.checkpoint.nodes[parent_id];
            let first_deeper_combination = (node.state.factors.len()
                < self.checkpoint.bindings.max_subset_size
                && node.children.is_empty())
            .then(|| {
                node.remaining_actions
                    .iter()
                    .position(|action| matches!(action, CexFactorSubsetActionV1::Add { .. }))
            })
            .flatten();
            let action_index = first_deeper_combination.unwrap_or_else(|| {
                self.checkpoint
                    .rng
                    .index(self.checkpoint.nodes[parent_id].remaining_actions.len())
            });
            let action = self.checkpoint.nodes[parent_id]
                .remaining_actions
                .remove(action_index);
            let state = apply_action(
                &self.checkpoint.nodes[parent_id].state,
                &action,
                &self.factor_bank,
                self.checkpoint.bindings.max_subset_size,
            )?;
            let state_sha256 = state.content_hash()?;
            self.checkpoint.expansions_used += 1;
            new_transitions += 1;
            let step = self.checkpoint.expansions_used;
            if !self
                .checkpoint
                .seen_state_sha256
                .insert(state_sha256.clone())
            {
                self.checkpoint.trace.push(CexFactorBankMctsTraceStepV1 {
                    step,
                    parent_node_id: parent_id,
                    action,
                    resulting_state: state,
                    outcome: TraceOutcomeV1::Duplicate { state_sha256 },
                });
                self.refresh_subtree_expandability(parent_id)?;
                continue;
            }

            let evaluation = evaluate_subset(context, &self.factor_bank, &state, &self.evaluator)?;
            let reward = evaluation.score;
            let evaluation_sha256 =
                canonical_json_hash(&evaluation).map_err(|error| error.to_string())?;
            let node_id = self.checkpoint.nodes.len();
            let depth = self.checkpoint.nodes[parent_id].depth + 1;
            let remaining_actions = legal_actions(
                &state,
                &self.factor_bank,
                self.checkpoint.bindings.max_subset_size,
            )?;
            self.checkpoint.nodes.push(NodeV1 {
                state: state.clone(),
                parent: Some(parent_id),
                children: vec![],
                subtree_has_expansion: !remaining_actions.is_empty(),
                remaining_actions,
                action: Some(action.clone()),
                depth,
                visits: 0,
                total_reward: 0.0,
                best_reward: None,
                evaluation: Some(evaluation),
            });
            self.checkpoint.nodes[parent_id].children.push(node_id);
            self.refresh_subtree_expandability(parent_id)?;
            backpropagate_lineage(&mut self.checkpoint.nodes, 0, node_id, reward)
                .map_err(|error| error.to_string())?;
            self.checkpoint.candidates_evaluated += 1;
            if self.is_better_selection(node_id)? {
                self.checkpoint.selected_node_id = Some(node_id);
            }
            self.checkpoint.trace.push(CexFactorBankMctsTraceStepV1 {
                step,
                parent_node_id: parent_id,
                action,
                resulting_state: state,
                outcome: TraceOutcomeV1::Evaluated {
                    node_id,
                    reward,
                    evaluation_sha256,
                },
            });
        }
    }

    pub fn checkpoint(&self) -> Result<CexFactorBankMctsCheckpointV1, String> {
        self.validate_checkpoint()?;
        Ok(self.checkpoint.clone())
    }

    pub fn trace(&self) -> Result<Vec<CexFactorBankMctsTraceStepV1>, String> {
        self.validate_checkpoint()?;
        Ok(self.checkpoint.trace.clone())
    }

    pub fn result(&self) -> Result<CexFactorBankMctsResultV1, String> {
        self.validate_checkpoint()?;
        let terminal_reason = self
            .checkpoint
            .terminal_reason
            .ok_or_else(|| "Factor-Bank MCTS search is not terminal".to_string())?;
        let selected = match self.selected_node()? {
            Some(node) => Some(CexFactorBankMctsSelectionV1 {
                subset: node.state.clone(),
                normalized_equal_abs_weight: 1.0 / node.state.factors.len() as f64,
                evaluation: node.evaluation.clone().ok_or_else(|| {
                    "Factor-Bank MCTS selected node has no evaluation".to_string()
                })?,
            }),
            None => None,
        };
        Ok(CexFactorBankMctsResultV1 {
            schema_version: RESULT_SCHEMA_VERSION.to_string(),
            implementation_version: CEX_FACTOR_BANK_MCTS_IMPLEMENTATION_VERSION.to_string(),
            mission_id: self.checkpoint.bindings.mission_id.clone(),
            factor_bank_revision_id: self.checkpoint.bindings.factor_bank_revision_id.clone(),
            baseline_gate_id: self.checkpoint.bindings.baseline_gate_id.clone(),
            terminal_reason,
            expansions_used: self.checkpoint.expansions_used,
            candidates_evaluated: self.checkpoint.candidates_evaluated,
            selected,
            checkpoint_sha256: self.checkpoint.content_hash()?,
        })
    }

    pub fn combination_artifact(
        &self,
        mission: &CexResearchMissionArtifactV1,
        ridge: &CexBaselineArtifactV1,
        cart: &CexBaselineArtifactV1,
        gate: &CexBaselineGateV1,
    ) -> Result<Option<CexCombinationResearchArtifactV1>, String> {
        self.validate_checkpoint()?;
        let result = self.result()?;
        if result.selected.is_none() {
            return Ok(None);
        }
        CexCombinationResearchArtifactV1::from_validated_search(
            mission,
            &self.factor_bank,
            ridge,
            cart,
            gate,
            &result,
            &self.checkpoint,
        )
        .map(Some)
    }

    fn selected_node(&self) -> Result<Option<&NodeV1>, String> {
        self.checkpoint
            .selected_node_id
            .map(|node_id| {
                self.checkpoint
                    .nodes
                    .get(node_id)
                    .ok_or_else(|| "Factor-Bank MCTS selected node is missing".to_string())
            })
            .transpose()
    }

    fn is_better_selection(&self, node_id: usize) -> Result<bool, String> {
        let candidate = self
            .checkpoint
            .nodes
            .get(node_id)
            .ok_or_else(|| "Factor-Bank MCTS candidate node is missing".to_string())?;
        let candidate_evaluation = candidate
            .evaluation
            .as_ref()
            .ok_or_else(|| "Factor-Bank MCTS candidate evaluation is missing".to_string())?;
        if !candidate_evaluation.passed {
            return Ok(false);
        }
        let candidate_score = candidate_evaluation.score;
        let Some(selected_id) = self.checkpoint.selected_node_id else {
            return Ok(true);
        };
        let selected = &self.checkpoint.nodes[selected_id];
        let selected_score = selected
            .evaluation
            .as_ref()
            .ok_or_else(|| "Factor-Bank MCTS selected evaluation is missing".to_string())?
            .score;
        Ok(candidate_score.total_cmp(&selected_score).is_gt()
            || (candidate_score.total_cmp(&selected_score).is_eq()
                && candidate.state < selected.state))
    }

    fn expected_terminal_reason(&self) -> Result<Option<CexFactorBankMctsStopReasonV1>, String> {
        if let Some(reason) = self.budget_terminal_reason() {
            return Ok(Some(reason));
        }
        Ok(
            select_expandable_progressively(&self.checkpoint.nodes, 0, UCT_EXPLORATION)
                .map_err(|error| error.to_string())?
                .is_none()
                .then_some(CexFactorBankMctsStopReasonV1::SearchSpaceExhausted),
        )
    }

    fn budget_terminal_reason(&self) -> Option<CexFactorBankMctsStopReasonV1> {
        if self.checkpoint.candidates_evaluated >= self.checkpoint.bindings.budget.max_candidates {
            Some(CexFactorBankMctsStopReasonV1::CandidateBudgetExhausted)
        } else if self.checkpoint.expansions_used >= self.checkpoint.bindings.budget.max_expansions
        {
            Some(CexFactorBankMctsStopReasonV1::ExpansionBudgetExhausted)
        } else {
            None
        }
    }

    fn refresh_subtree_expandability(&mut self, mut node_id: usize) -> Result<(), String> {
        loop {
            let node = self
                .checkpoint
                .nodes
                .get(node_id)
                .ok_or_else(|| "Factor-Bank MCTS expansion lineage is missing".to_string())?;
            let expected = !node.remaining_actions.is_empty()
                || node
                    .children
                    .iter()
                    .any(|&child_id| self.checkpoint.nodes[child_id].subtree_has_expansion);
            if node.subtree_has_expansion == expected {
                return Ok(());
            }
            let parent = node.parent;
            self.checkpoint.nodes[node_id].subtree_has_expansion = expected;
            let Some(parent_id) = parent else {
                return Ok(());
            };
            node_id = parent_id;
        }
    }

    fn validate_checkpoint(&self) -> Result<(), String> {
        let checkpoint = &self.checkpoint;
        if checkpoint.schema_version != CHECKPOINT_SCHEMA_VERSION {
            return Err("Factor-Bank MCTS checkpoint schema is invalid".to_string());
        }
        checkpoint.bindings.validate()?;
        self.factor_bank
            .validate()
            .map_err(|error| error.to_string())?;
        if checkpoint.candidates_evaluated > checkpoint.bindings.budget.max_candidates
            || checkpoint.expansions_used > checkpoint.bindings.budget.max_expansions
        {
            return Err("Factor-Bank MCTS checkpoint exceeds its frozen budget".to_string());
        }
        if checkpoint.bindings.factor_bank_revision_id != self.factor_bank.revision_id
            || checkpoint.bindings.max_subset_size != self.factor_bank.entries.len()
            || checkpoint.expansions_used != checkpoint.trace.len() as u64
            || checkpoint.candidates_evaluated + 1 != checkpoint.nodes.len()
        {
            return Err("Factor-Bank MCTS checkpoint identity is invalid".to_string());
        }
        validate_tree(
            &checkpoint.nodes,
            0,
            usize::try_from(checkpoint.bindings.budget.max_expansions).unwrap_or(usize::MAX),
        )
        .map_err(|error| error.to_string())?;
        let mut subtree_has_expansion = vec![false; checkpoint.nodes.len()];
        for node_id in (0..checkpoint.nodes.len()).rev() {
            let node = &checkpoint.nodes[node_id];
            let expected = !node.remaining_actions.is_empty()
                || node
                    .children
                    .iter()
                    .any(|&child_id| subtree_has_expansion[child_id]);
            if node.subtree_has_expansion != expected {
                return Err("Factor-Bank MCTS expansion index drifted".to_string());
            }
            subtree_has_expansion[node_id] = expected;
        }
        let identities = factor_identity_map(&self.factor_bank)?;
        let mut seen = BTreeSet::new();
        let mut legal_by_node = Vec::with_capacity(checkpoint.nodes.len());
        for (node_id, node) in checkpoint.nodes.iter().enumerate() {
            validate_state(
                &node.state,
                &identities,
                node_id == 0,
                checkpoint.bindings.max_subset_size,
            )?;
            if !seen.insert(node.state.content_hash()?) {
                return Err("Factor-Bank MCTS checkpoint contains duplicate states".to_string());
            }
            let legal = legal_actions(
                &node.state,
                &self.factor_bank,
                checkpoint.bindings.max_subset_size,
            )?
            .into_iter()
            .collect::<BTreeSet<_>>();
            if node.remaining_actions.iter().collect::<BTreeSet<_>>().len()
                != node.remaining_actions.len()
                || node
                    .remaining_actions
                    .iter()
                    .any(|action| !legal.contains(action))
            {
                return Err("Factor-Bank MCTS checkpoint actions are invalid".to_string());
            }
            legal_by_node.push(legal);
            if node_id == 0 {
                if node.parent.is_some() || node.action.is_some() || node.evaluation.is_some() {
                    return Err("Factor-Bank MCTS root is invalid".to_string());
                }
                continue;
            }
            let parent_id = node
                .parent
                .ok_or_else(|| "Factor-Bank MCTS node has no parent".to_string())?;
            let action = node
                .action
                .as_ref()
                .ok_or_else(|| "Factor-Bank MCTS node has no action".to_string())?;
            if apply_action(
                &checkpoint.nodes[parent_id].state,
                action,
                &self.factor_bank,
                checkpoint.bindings.max_subset_size,
            )? != node.state
            {
                return Err("Factor-Bank MCTS node transition drifted".to_string());
            }
            let evaluation = node
                .evaluation
                .as_ref()
                .ok_or_else(|| "Factor-Bank MCTS node has no evaluation".to_string())?;
            evaluation.validate().map_err(|error| error.to_string())?;
            if evaluation
                .protocol_binding()
                .map_err(|error| error.to_string())?
                .1
                != checkpoint.bindings.scoring_policy.content_sha256
                || canonical_json_hash(
                    &evaluation
                        .formula_config()
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?
                    != checkpoint.bindings.screening_policy.content_sha256
            {
                return Err("Factor-Bank MCTS evaluation policy drifted".to_string());
            }
        }
        if seen != checkpoint.seen_state_sha256 {
            return Err("Factor-Bank MCTS seen-state index drifted".to_string());
        }
        let mut consumed_actions = BTreeMap::<usize, BTreeSet<CexFactorSubsetActionV1>>::new();
        let mut traced_nodes = BTreeSet::new();
        for (index, step) in checkpoint.trace.iter().enumerate() {
            if step.step != index as u64 + 1
                || step.parent_node_id >= checkpoint.nodes.len()
                || apply_action(
                    &checkpoint.nodes[step.parent_node_id].state,
                    &step.action,
                    &self.factor_bank,
                    checkpoint.bindings.max_subset_size,
                )? != step.resulting_state
            {
                return Err("Factor-Bank MCTS trace drifted".to_string());
            }
            if !consumed_actions
                .entry(step.parent_node_id)
                .or_default()
                .insert(step.action.clone())
            {
                return Err("Factor-Bank MCTS trace repeats an action".to_string());
            }
            match &step.outcome {
                TraceOutcomeV1::Evaluated {
                    node_id,
                    reward,
                    evaluation_sha256,
                } => {
                    let node = checkpoint
                        .nodes
                        .get(*node_id)
                        .ok_or_else(|| "Factor-Bank MCTS trace node is missing".to_string())?;
                    let evaluation = node.evaluation.as_ref().ok_or_else(|| {
                        "Factor-Bank MCTS trace evaluation is missing".to_string()
                    })?;
                    if *node_id == 0
                        || !traced_nodes.insert(*node_id)
                        || node.parent != Some(step.parent_node_id)
                        || node.action.as_ref() != Some(&step.action)
                        || node.state != step.resulting_state
                        || reward.to_bits() != evaluation.score.to_bits()
                        || *evaluation_sha256
                            != canonical_json_hash(evaluation).map_err(|error| error.to_string())?
                    {
                        return Err("Factor-Bank MCTS trace evaluation drifted".to_string());
                    }
                }
                TraceOutcomeV1::Duplicate { state_sha256 }
                    if *state_sha256 == step.resulting_state.content_hash()? => {}
                TraceOutcomeV1::Duplicate { .. } => {
                    return Err("Factor-Bank MCTS duplicate trace drifted".to_string());
                }
            }
        }
        if traced_nodes != (1..checkpoint.nodes.len()).collect() {
            return Err("Factor-Bank MCTS trace does not bind every evaluated node".to_string());
        }
        for (node_id, node) in checkpoint.nodes.iter().enumerate() {
            let consumed = consumed_actions.get(&node_id).cloned().unwrap_or_default();
            let legal = &legal_by_node[node_id];
            let expected_remaining = legal.difference(&consumed).cloned().collect::<Vec<_>>();
            if !consumed.is_subset(legal) || node.remaining_actions != expected_remaining {
                return Err("Factor-Bank MCTS untried actions drifted".to_string());
            }
        }
        let selected = checkpoint.selected_node_id;
        let expected_selected = checkpoint
            .nodes
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, node)| {
                node.evaluation
                    .as_ref()
                    .is_some_and(|evaluation| evaluation.passed)
            })
            .min_by(|(_, left), (_, right)| {
                let left_score = left.evaluation.as_ref().expect("validated node").score;
                let right_score = right.evaluation.as_ref().expect("validated node").score;
                right_score
                    .total_cmp(&left_score)
                    .then_with(|| left.state.cmp(&right.state))
            })
            .map(|(node_id, _)| node_id);
        if selected != expected_selected {
            return Err("Factor-Bank MCTS selected subset drifted".to_string());
        }
        if checkpoint.terminal_reason.is_some()
            && checkpoint.terminal_reason != self.expected_terminal_reason()?
        {
            return Err("Factor-Bank MCTS terminal reason drifted".to_string());
        }
        Ok(())
    }
}

fn validate_start(
    mission: &CexResearchMissionArtifactV1,
    factor_bank: &CexFactorBankRevisionV2,
    ridge: &CexBaselineArtifactV1,
    cart: &CexBaselineArtifactV1,
    gate: &CexBaselineGateV1,
    context: &EngineContext<'_>,
) -> Result<(), String> {
    validate_source_bindings(mission, factor_bank, ridge, cart, gate)?;
    verify_cex_baseline_artifact(context, factor_bank, ridge)?;
    verify_cex_baseline_artifact(context, factor_bank, cart)?;
    validate_cex_context_bindings(context, factor_bank, &mission.spec.policies.evaluation)
}

fn validate_source_bindings(
    mission: &CexResearchMissionArtifactV1,
    factor_bank: &CexFactorBankRevisionV2,
    ridge: &CexBaselineArtifactV1,
    cart: &CexBaselineArtifactV1,
    gate: &CexBaselineGateV1,
) -> Result<(), String> {
    mission.validate().map_err(|error| error.to_string())?;
    factor_bank.validate().map_err(|error| error.to_string())?;
    ridge.validate().map_err(|error| error.to_string())?;
    cart.validate().map_err(|error| error.to_string())?;
    gate.validate().map_err(|error| error.to_string())?;
    CexEqualAbsoluteWeightPolicyV1::controlled_v1(mission.spec.policies.weight.id.clone())
        .and_then(|policy| policy.validate_binding(&mission.spec.policies.weight))
        .map_err(|error| error.to_string())?;
    if factor_bank.entries.is_empty()
        || factor_bank.search_lineage_id != mission.spec.search_lineage_id
        || factor_bank
            .gp_policy
            .validate_binding(&mission.spec.policies.gp)
            .is_err()
        || factor_bank.screening_policy != mission.spec.policies.screening
        || factor_bank.evaluation_policy != mission.spec.policies.evaluation
        || ridge.baseline_policy != cart.baseline_policy
    {
        return Err("Factor-Bank MCTS producer bindings drifted".to_string());
    }
    ridge
        .validate_binding(mission, &ridge.baseline_policy, factor_bank)
        .map_err(|error| error.to_string())?;
    cart.validate_binding(mission, &cart.baseline_policy, factor_bank)
        .map_err(|error| error.to_string())?;
    gate.validate_binding(
        mission,
        &ridge.baseline_policy,
        factor_bank,
        Some(ridge),
        Some(cart),
    )
    .map_err(|error| error.to_string())?;
    if !gate.passed || !ridge.evaluation.passed || !cart.evaluation.passed {
        return Err("Factor-Bank MCTS requires both passing baseline artifacts".to_string());
    }
    Ok(())
}

fn validate_combination_sources(
    mission: &CexResearchMissionArtifactV1,
    factor_bank: &CexFactorBankRevisionV2,
    ridge: &CexBaselineArtifactV1,
    cart: &CexBaselineArtifactV1,
    gate: &CexBaselineGateV1,
    result: &CexFactorBankMctsResultV1,
) -> Result<(), String> {
    validate_source_bindings(mission, factor_bank, ridge, cart, gate)?;
    let selection = result
        .selected
        .as_ref()
        .ok_or_else(|| "CEX combination strategy requires a passing selected subset".to_string())?;
    let identities = factor_identity_map(factor_bank)?;
    validate_state(
        &selection.subset,
        &identities,
        false,
        factor_bank.entries.len(),
    )?;
    selection
        .evaluation
        .validate()
        .map_err(|error| error.to_string())?;
    let evaluator_config = selection
        .evaluation
        .formula_config()
        .map_err(|error| error.to_string())?;
    let expected_weight = 1.0 / selection.subset.factors.len() as f64;
    if result.schema_version != RESULT_SCHEMA_VERSION
        || result.implementation_version != CEX_FACTOR_BANK_MCTS_IMPLEMENTATION_VERSION
        || result.mission_id != mission.semantic_id().map_err(|error| error.to_string())?
        || result.factor_bank_revision_id != factor_bank.revision_id
        || result.baseline_gate_id != gate.gate_id
        || result.terminal_reason == CexFactorBankMctsStopReasonV1::Paused
        || result.expansions_used == 0
        || result.candidates_evaluated == 0
        || u64::try_from(result.candidates_evaluated)
            .map_or(true, |count| count > result.expansions_used)
        || !valid_sha256(&result.checkpoint_sha256)
        || !selection.evaluation.passed
        || selection.evaluation.evaluator_version != WALK_FORWARD_EVALUATOR_VERSION
        || selection.normalized_equal_abs_weight.to_bits() != expected_weight.to_bits()
        || selection
            .evaluation
            .protocol_binding()
            .map_or(true, |(_, hash)| {
                hash != mission.spec.policies.evaluation.content_sha256
            })
        || canonical_json_hash(&evaluator_config).map_err(|error| error.to_string())?
            != mission.spec.policies.screening.content_sha256
        || ridge.model_kind != CexBaselineModelKindV1::Ridge
        || cart.model_kind != CexBaselineModelKindV1::ShallowCart
    {
        return Err("CEX combination source evidence is invalid".to_string());
    }
    Ok(())
}

fn factor_identities(
    factor_bank: &CexFactorBankRevisionV2,
) -> Result<Vec<CexFactorIdentityV1>, String> {
    let mut identities = factor_bank
        .entries
        .iter()
        .map(|entry| {
            Ok(CexFactorIdentityV1 {
                factor_id: entry.factor_id.clone(),
                content_sha256: canonical_json_hash(entry).map_err(|error| error.to_string())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    identities.sort();
    if identities
        .windows(2)
        .any(|pair| pair[0].factor_id == pair[1].factor_id)
    {
        return Err("Factor Bank contains duplicate Factor IDs".to_string());
    }
    Ok(identities)
}

fn factor_identity_map(
    factor_bank: &CexFactorBankRevisionV2,
) -> Result<BTreeMap<String, String>, String> {
    factor_identities(factor_bank).map(|identities| {
        identities
            .into_iter()
            .map(|identity| (identity.factor_id, identity.content_sha256))
            .collect()
    })
}

fn validate_state(
    state: &CexFactorSubsetV1,
    identities: &BTreeMap<String, String>,
    allow_empty: bool,
    max_subset_size: usize,
) -> Result<(), String> {
    if (!allow_empty && state.factors.is_empty())
        || state.factors.len() > max_subset_size
        || state.factors.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err("Factor-Bank MCTS subset is not canonical".to_string());
    }
    for factor in &state.factors {
        match identities.get(&factor.factor_id) {
            Some(content_sha256) if content_sha256 == &factor.content_sha256 => {}
            Some(_) => return Err("Factor-Bank MCTS factor content hash drifted".to_string()),
            None => {
                return Err("Factor-Bank MCTS references an unknown or rejected factor".to_string())
            }
        }
    }
    Ok(())
}

fn legal_actions(
    state: &CexFactorSubsetV1,
    factor_bank: &CexFactorBankRevisionV2,
    max_subset_size: usize,
) -> Result<Vec<CexFactorSubsetActionV1>, String> {
    let identities = factor_identities(factor_bank)?;
    if state.factors.is_empty() {
        let factor = identities
            .into_iter()
            .next()
            .ok_or_else(|| "Factor Bank has no bootstrap factor".to_string())?;
        return Ok(vec![CexFactorSubsetActionV1::Add {
            factor_bank_revision_id: factor_bank.revision_id.clone(),
            factor,
        }]);
    }
    let present = state.factors.iter().collect::<BTreeSet<_>>();
    let mut actions = Vec::new();
    if state.factors.len() < max_subset_size {
        actions.extend(
            identities
                .iter()
                .filter(|factor| !present.contains(factor))
                .cloned()
                .map(|factor| CexFactorSubsetActionV1::Add {
                    factor_bank_revision_id: factor_bank.revision_id.clone(),
                    factor,
                }),
        );
    }
    if state.factors.len() > 1 {
        actions.extend(state.factors.iter().cloned().map(|factor| {
            CexFactorSubsetActionV1::Remove {
                factor_bank_revision_id: factor_bank.revision_id.clone(),
                factor,
            }
        }));
    }
    // ponytail: swap enumeration is O(n²); index it only if Factor Banks become large enough to measure.
    if !state.factors.is_empty() {
        for remove in &state.factors {
            for add in identities.iter().filter(|factor| !present.contains(factor)) {
                actions.push(CexFactorSubsetActionV1::Swap {
                    factor_bank_revision_id: factor_bank.revision_id.clone(),
                    remove: remove.clone(),
                    add: add.clone(),
                });
            }
        }
    }
    actions.sort();
    Ok(actions)
}

fn apply_action(
    state: &CexFactorSubsetV1,
    action: &CexFactorSubsetActionV1,
    factor_bank: &CexFactorBankRevisionV2,
    max_subset_size: usize,
) -> Result<CexFactorSubsetV1, String> {
    if action.factor_bank_revision_id() != factor_bank.revision_id {
        return Err("Factor-Bank MCTS action crosses Factor Bank revisions".to_string());
    }
    let identities = factor_identity_map(factor_bank)?;
    validate_state(
        state,
        &identities,
        state.factors.is_empty(),
        max_subset_size,
    )?;
    let mut factors = state.factors.clone();
    let position = |values: &[CexFactorIdentityV1], factor: &CexFactorIdentityV1| {
        values.iter().position(|value| value == factor)
    };
    match action {
        CexFactorSubsetActionV1::Add { factor, .. } => {
            validate_state(
                &CexFactorSubsetV1 {
                    factors: vec![factor.clone()],
                },
                &identities,
                false,
                max_subset_size,
            )?;
            if factors.len() >= max_subset_size || position(&factors, factor).is_some() {
                return Err("Factor-Bank MCTS add action is illegal".to_string());
            }
            factors.push(factor.clone());
        }
        CexFactorSubsetActionV1::Remove { factor, .. } => {
            if factors.len() <= 1 {
                return Err("Factor-Bank MCTS remove action violates subset limits".to_string());
            }
            let index = position(&factors, factor)
                .ok_or_else(|| "Factor-Bank MCTS remove factor is absent".to_string())?;
            factors.remove(index);
        }
        CexFactorSubsetActionV1::Swap { remove, add, .. } => {
            validate_state(
                &CexFactorSubsetV1 {
                    factors: vec![add.clone()],
                },
                &identities,
                false,
                max_subset_size,
            )?;
            let index = position(&factors, remove)
                .ok_or_else(|| "Factor-Bank MCTS swap source is absent".to_string())?;
            if remove == add || position(&factors, add).is_some() {
                return Err("Factor-Bank MCTS swap target is illegal".to_string());
            }
            factors[index] = add.clone();
        }
    }
    factors.sort();
    let next = CexFactorSubsetV1 { factors };
    validate_state(&next, &identities, false, max_subset_size)?;
    Ok(next)
}

fn evaluate_subset(
    context: &EngineContext<'_>,
    factor_bank: &CexFactorBankRevisionV2,
    state: &CexFactorSubsetV1,
    evaluator: &FormulaEvaluator,
) -> Result<CandidateEvaluation, String> {
    let by_id = factor_bank
        .entries
        .iter()
        .map(|entry| (entry.factor_id.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let entries = state
        .factors
        .iter()
        .map(|factor| {
            by_id
                .get(factor.factor_id.as_str())
                .copied()
                .ok_or_else(|| "Factor-Bank MCTS selected an unknown factor".to_string())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let factors = evaluate_factor_features_from_entries(context, &entries)?;
    let weight = 1.0 / factors.len() as f64;
    let mut signals = vec![0.0; context.rows().len()];
    for factor in factors {
        for (signal, value) in signals.iter_mut().zip(factor) {
            *signal += value * weight;
        }
    }
    evaluator.evaluate_signals(
        context.rows(),
        &signals,
        context.folds().iter().map(|fold| fold.validation.clone()),
        WALK_FORWARD_EVALUATOR_VERSION,
        context.protocol(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_schema_rejects_formula_gp_weight_and_continuous_parameters() {
        for forbidden in ["formula", "gp", "orientation", "weight", "threshold"] {
            let mut value = serde_json::json!({
                "action": "add",
                "factor_bank_revision_id": "bank-1",
                "factor": { "factor_id": "factor-1", "content_sha256": "a".repeat(64) },
            });
            value
                .as_object_mut()
                .unwrap()
                .insert(forbidden.to_string(), serde_json::json!(1.0));
            assert!(serde_json::from_value::<CexFactorSubsetActionV1>(value).is_err());
        }
    }

    #[test]
    fn result_schema_preserves_a_terminal_no_selection_outcome() {
        let result = CexFactorBankMctsResultV1 {
            schema_version: RESULT_SCHEMA_VERSION.to_string(),
            implementation_version: CEX_FACTOR_BANK_MCTS_IMPLEMENTATION_VERSION.to_string(),
            mission_id: "mission-1".to_string(),
            factor_bank_revision_id: "bank-1".to_string(),
            baseline_gate_id: "gate-1".to_string(),
            terminal_reason: CexFactorBankMctsStopReasonV1::CandidateBudgetExhausted,
            expansions_used: 1,
            candidates_evaluated: 1,
            selected: None,
            checkpoint_sha256: "a".repeat(64),
        };

        assert!(serde_json::to_value(result).unwrap()["selected"].is_null());
    }
}
