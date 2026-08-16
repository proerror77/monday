use super::DeterministicRng;
use crate::{
    baselines::{
        evaluate_factor_features_from_entries, validate_cex_context_bindings,
        verify_cex_baseline_artifact,
    },
    evaluation::EngineContext,
    formula_evaluator::FormulaEvaluator,
    CandidateEvaluation,
};
use alpha_domain::{
    canonical_json_hash, CexBaselineArtifactV1, CexBaselineGateV1, CexEqualAbsoluteWeightPolicyV1,
    CexFactorBankRevisionV2, CexResearchContentRefV1, CexResearchMissionArtifactV1,
    FormulaEvaluatorConfig, SearchBudget, WALK_FORWARD_EVALUATOR_VERSION,
};
use hft_search_kernel::{
    backpropagate_lineage, select_expandable_progressively, validate_tree, UctNode, UctStats,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const CEX_FACTOR_BANK_MCTS_IMPLEMENTATION_VERSION: &str = "cex-factor-bank-subset-mcts-v1";
const CHECKPOINT_SCHEMA_VERSION: &str = "cex-factor-bank-subset-mcts-checkpoint-v1";
const RESULT_SCHEMA_VERSION: &str = "cex-factor-bank-subset-mcts-result-v1";
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
    verify_cex_baseline_artifact(context, factor_bank, ridge)?;
    verify_cex_baseline_artifact(context, factor_bank, cart)?;
    validate_cex_context_bindings(context, factor_bank, &mission.spec.policies.evaluation)
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
