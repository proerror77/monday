use super::DeterministicRng;
use crate::{
    evaluation::ProposalContext, CandidateEvaluation, EngineProposal, HistoricalObservation,
    ProposalEngine, ProposalEngineCheckpoint, RemainingBudget,
};
use alpha_domain::{
    CandidateArtifact, EngineKind, MissionCompletionPolicy, ResearchMission, SearchBudget,
    WALK_FORWARD_EVALUATOR_VERSION,
};
use hft_factor_dsl::{validate_live_formula, FactorAst, FactorOperator, FactorTerminal};
use hft_search_kernel::{backpropagate, select_expandable, validate_tree, UctNode, UctStats};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MCTS_CHECKPOINT_VERSION: u32 = 3;
const CEX_MCTS_SEARCH_IDENTITY_VERSION: &str = "cex-mcts-search-identity-v1";
const CEX_MCTS_CANDIDATE_SPACE_ID: &str = "live-factor-ast-add-secondary-v1";
const CEX_MCTS_STOPPING_RULE_ID: &str = "min-kept-search-budget-or-max-new-iterations-pause-v1";
const CEX_MCTS_SELECTION_RULE_ID: &str = "one-canonical-walk-forward-candidate-v1";
const CEX_MCTS_UCT_TIE_BREAK_RULE_ID: &str = "last-child-on-total-cmp-tie-v1";

fn expansion_actions(live_only: bool) -> Vec<usize> {
    if live_only {
        vec![3]
    } else {
        vec![0, 1, 2, 3]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    ast: FactorAst,
    parent: Option<usize>,
    children: Vec<usize>,
    unexpanded_actions: Vec<usize>,
    depth: usize,
    visits: u64,
    total_reward: f64,
    best_reward: Option<f64>,
}

impl UctNode for Node {
    fn parent(&self) -> Option<usize> {
        self.parent
    }

    fn children(&self) -> &[usize] {
        &self.children
    }

    fn is_expandable(&self) -> bool {
        !self.unexpanded_actions.is_empty()
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MctsConfigV3 {
    seed: u64,
    root_ast: FactorAst,
    secondary_field: String,
    exploration: f64,
    max_depth: usize,
    live_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    research_identity: Option<CexMctsSearchIdentityV1>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MctsCheckpointV3 {
    config: MctsConfigV3,
    rng: DeterministicRng,
    nodes: Vec<Node>,
    candidates: BTreeMap<String, usize>,
    seen: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexMctsSearchIdentityV1 {
    pub schema_version: String,
    pub mission_id: String,
    pub dataset_manifest_id: String,
    pub search_policy_snapshot_id: String,
    pub search_budget: SearchBudget,
    pub completion_policy: MissionCompletionPolicy,
    pub max_new_iterations: Option<usize>,
    pub evaluator_version: String,
    pub evaluator_config_hash: String,
    pub evaluation_protocol_hash: String,
    pub candidate_space_id: String,
    pub stopping_rule_id: String,
    pub selection_rule_id: String,
    pub uct_tie_break_rule_id: String,
}

impl CexMctsSearchIdentityV1 {
    pub fn for_mission(
        mission: &ResearchMission,
        evaluation_protocol_hash: String,
        evaluator_config_hash: String,
        max_new_iterations: Option<usize>,
    ) -> Result<Self, String> {
        mission.validate().map_err(|error| error.to_string())?;
        let identity = Self {
            schema_version: CEX_MCTS_SEARCH_IDENTITY_VERSION.to_string(),
            mission_id: mission.mission_id.clone(),
            dataset_manifest_id: mission.dataset_manifest_id.as_str().to_string(),
            search_policy_snapshot_id: mission.search_policy_snapshot_id.clone(),
            search_budget: mission.search_budget.clone(),
            completion_policy: mission.completion_policy.clone(),
            max_new_iterations,
            evaluator_version: WALK_FORWARD_EVALUATOR_VERSION.to_string(),
            evaluator_config_hash,
            evaluation_protocol_hash,
            candidate_space_id: CEX_MCTS_CANDIDATE_SPACE_ID.to_string(),
            stopping_rule_id: CEX_MCTS_STOPPING_RULE_ID.to_string(),
            selection_rule_id: CEX_MCTS_SELECTION_RULE_ID.to_string(),
            uct_tie_break_rule_id: CEX_MCTS_UCT_TIE_BREAK_RULE_ID.to_string(),
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != CEX_MCTS_SEARCH_IDENTITY_VERSION
            || self.mission_id.trim().is_empty()
            || self.dataset_manifest_id.trim().is_empty()
            || self.search_policy_snapshot_id.trim().is_empty()
            || self.evaluator_version != WALK_FORWARD_EVALUATOR_VERSION
            || self.candidate_space_id != CEX_MCTS_CANDIDATE_SPACE_ID
            || self.stopping_rule_id != CEX_MCTS_STOPPING_RULE_ID
            || self.selection_rule_id != CEX_MCTS_SELECTION_RULE_ID
            || self.uct_tie_break_rule_id != CEX_MCTS_UCT_TIE_BREAK_RULE_ID
            || self.max_new_iterations == Some(0)
            || !is_sha256(&self.evaluator_config_hash)
            || !is_sha256(&self.evaluation_protocol_hash)
        {
            return Err("invalid CEX MCTS research identity".to_string());
        }
        self.search_budget
            .validate()
            .map_err(|error| error.to_string())?;
        self.completion_policy
            .validate()
            .map_err(|error| error.to_string())
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Clone, PartialEq)]
pub struct MctsNodeSnapshot {
    pub node_id: usize,
    pub parent_id: Option<usize>,
    pub visits: u64,
    pub total_reward: f64,
    pub best_reward: Option<f64>,
    pub formula: String,
}

pub struct MctsEngine {
    rng: DeterministicRng,
    seed: u64,
    exploration: f64,
    max_depth: usize,
    secondary_field: String,
    live_only: bool,
    research_identity: Option<CexMctsSearchIdentityV1>,
    nodes: Vec<Node>,
    candidates: BTreeMap<String, usize>,
    seen: BTreeSet<String>,
}

impl MctsEngine {
    pub fn new(
        seed: u64,
        root_field: impl Into<String>,
        secondary_field: impl Into<String>,
        exploration: f64,
        max_depth: usize,
    ) -> Result<Self, String> {
        Self::new_with_mode(
            seed,
            root_field,
            secondary_field,
            exploration,
            max_depth,
            false,
        )
    }

    pub fn new_live(
        seed: u64,
        root_field: impl Into<String>,
        secondary_field: impl Into<String>,
        exploration: f64,
        max_depth: usize,
    ) -> Result<Self, String> {
        let root_field = root_field.into();
        let secondary_field = secondary_field.into();
        let live_seed = FactorAst::call(
            FactorOperator::Add,
            vec![
                FactorAst::Terminal(FactorTerminal::Field(root_field.clone())),
                FactorAst::Terminal(FactorTerminal::Field(secondary_field.clone())),
            ],
        )
        .map_err(|error| format!("invalid live MCTS fields: {error}"))?;
        validate_live_formula(&live_seed)
            .map_err(|error| format!("invalid live MCTS fields: {error}"))?;
        Self::new_with_mode(
            seed,
            root_field,
            secondary_field,
            exploration,
            max_depth,
            true,
        )
    }

    pub fn new_live_bound(
        seed: u64,
        root_field: impl Into<String>,
        secondary_field: impl Into<String>,
        exploration: f64,
        max_depth: usize,
        research_identity: CexMctsSearchIdentityV1,
    ) -> Result<Self, String> {
        research_identity.validate()?;
        let mut engine = Self::new_live(seed, root_field, secondary_field, exploration, max_depth)?;
        engine.research_identity = Some(research_identity);
        Ok(engine)
    }

    fn new_with_mode(
        seed: u64,
        root_field: impl Into<String>,
        secondary_field: impl Into<String>,
        exploration: f64,
        max_depth: usize,
        live_only: bool,
    ) -> Result<Self, String> {
        let seed = seed.max(1);
        let root_field = root_field.into();
        let secondary_field = secondary_field.into();
        if root_field.trim().is_empty() || secondary_field.trim().is_empty() {
            return Err("MCTS fields cannot be empty".to_string());
        }
        if !exploration.is_finite() || exploration < 0.0 || max_depth == 0 {
            return Err("invalid MCTS configuration".to_string());
        }
        Ok(Self {
            rng: DeterministicRng::new(seed),
            seed,
            exploration,
            max_depth,
            secondary_field,
            live_only,
            research_identity: None,
            nodes: vec![Node {
                ast: FactorAst::Terminal(FactorTerminal::Field(root_field)),
                parent: None,
                children: vec![],
                unexpanded_actions: expansion_actions(live_only),
                depth: 0,
                visits: 0,
                total_reward: 0.0,
                best_reward: None,
            }],
            candidates: BTreeMap::new(),
            seen: BTreeSet::new(),
        })
    }

    pub fn trace(&self) -> Vec<MctsNodeSnapshot> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(node_id, node)| MctsNodeSnapshot {
                node_id,
                parent_id: node.parent,
                visits: node.visits,
                total_reward: node.total_reward,
                best_reward: node.best_reward,
                formula: node.ast.to_string(),
            })
            .collect()
    }

    fn config(&self) -> Result<MctsConfigV3, String> {
        Ok(MctsConfigV3 {
            seed: self.seed,
            root_ast: self
                .nodes
                .first()
                .ok_or_else(|| "MCTS checkpoint has no root node".to_string())?
                .ast
                .clone(),
            secondary_field: self.secondary_field.clone(),
            exploration: self.exploration,
            max_depth: self.max_depth,
            live_only: self.live_only,
            research_identity: self.research_identity.clone(),
        })
    }

    fn expand(&mut self, parent_id: usize) -> Result<usize, String> {
        let action_index = self
            .rng
            .index(self.nodes[parent_id].unexpanded_actions.len());
        let action = self.nodes[parent_id]
            .unexpanded_actions
            .swap_remove(action_index);
        let parent_ast = self.nodes[parent_id].ast.clone();
        let ast = match action {
            0 => FactorAst::call(FactorOperator::Rank, vec![parent_ast]),
            1 => FactorAst::call(FactorOperator::Delta, vec![parent_ast, constant("5")]),
            2 => FactorAst::call(FactorOperator::Mean, vec![parent_ast, constant("20")]),
            _ => FactorAst::call(
                FactorOperator::Add,
                vec![
                    parent_ast,
                    FactorAst::Terminal(FactorTerminal::Field(self.secondary_field.clone())),
                ],
            ),
        }
        .map_err(|error| error.to_string())?;
        ast.validate().map_err(|error| error.to_string())?;
        let depth = self.nodes[parent_id].depth + 1;
        let node_id = self.nodes.len();
        self.nodes.push(Node {
            ast,
            parent: Some(parent_id),
            children: vec![],
            unexpanded_actions: if depth < self.max_depth {
                expansion_actions(self.live_only)
            } else {
                vec![]
            },
            depth,
            visits: 0,
            total_reward: 0.0,
            best_reward: None,
        });
        self.nodes[parent_id].children.push(node_id);
        Ok(node_id)
    }
}

impl ProposalEngine for MctsEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Mcts
    }

    fn propose(
        &mut self,
        mission_id: &str,
        iteration_index: usize,
        _context: &ProposalContext<'_>,
        remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String> {
        if remaining.expansions == 0 {
            return Err("MCTS expansion budget is exhausted".to_string());
        }
        for attempt in 1..=remaining.expansions.min(256) {
            let parent = select_expandable(&self.nodes, 0, self.exploration)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "MCTS tree has no expandable node".to_string())?;
            let node_id = self.expand(parent)?;
            let formula = self.nodes[node_id].ast.to_string();
            if self.seen.insert(formula) {
                let candidate_id = format!("{mission_id}-mcts-{iteration_index}");
                self.candidates.insert(candidate_id.clone(), node_id);
                return Ok(EngineProposal {
                    candidate_id,
                    hypothesis: "MCTS grammar expansion selected by UCT".to_string(),
                    artifact: CandidateArtifact::Formula(self.nodes[node_id].ast.clone()),
                    expansions: attempt,
                    tokens: 0,
                    elapsed_ms: 0,
                });
            }
        }
        Err("MCTS could not produce a novel candidate within budget".to_string())
    }

    fn observe(&mut self, proposal: &EngineProposal, evaluation: &CandidateEvaluation) {
        let Some(node_id) = self.candidates.remove(&proposal.candidate_id) else {
            return;
        };
        if !evaluation.score.is_finite() {
            return;
        }
        let _ = backpropagate(&mut self.nodes, 0, node_id, evaluation.score);
    }

    fn abandon(&mut self, proposal: &EngineProposal) {
        self.candidates.remove(&proposal.candidate_id);
    }

    fn restore(&mut self, observations: &[HistoricalObservation]) -> Result<(), String> {
        let mut nodes = self.nodes.clone();
        let mut seen = self.seen.clone();
        for observation in observations {
            let CandidateArtifact::Formula(ast) = &observation.proposal.artifact else {
                return Err("MCTS history contains a non-formula artifact".to_string());
            };
            if !observation.evaluation.score.is_finite() {
                return Err("MCTS history contains a non-finite score".to_string());
            }
            seen.insert(ast.to_string());
            backpropagate(&mut nodes, 0, 0, observation.evaluation.score)
                .map_err(|error| format!("invalid MCTS history: {error}"))?;
        }
        self.nodes = nodes;
        self.seen = seen;
        Ok(())
    }

    fn checkpoint(&self) -> Result<ProposalEngineCheckpoint, String> {
        let state = MctsCheckpointV3 {
            config: self.config()?,
            rng: self.rng.clone(),
            nodes: self.nodes.clone(),
            candidates: self.candidates.clone(),
            seen: self.seen.clone(),
        };
        state.validate()?;
        Ok(ProposalEngineCheckpoint {
            kind: EngineKind::Mcts,
            version: MCTS_CHECKPOINT_VERSION,
            state: serde_json::to_value(state)
                .map_err(|error| format!("failed to encode MCTS checkpoint: {error}"))?,
        })
    }

    fn restore_checkpoint(
        &mut self,
        checkpoint: &ProposalEngineCheckpoint,
        _observations: &[HistoricalObservation],
    ) -> Result<(), String> {
        if checkpoint.kind != EngineKind::Mcts || checkpoint.version != MCTS_CHECKPOINT_VERSION {
            return Err("MCTS checkpoint kind or version mismatch".to_string());
        }
        let state: MctsCheckpointV3 = serde_json::from_value(checkpoint.state.clone())
            .map_err(|error| format!("invalid MCTS checkpoint state: {error}"))?;
        state.validate()?;
        if state.config != self.config()? {
            return Err("MCTS checkpoint configuration mismatch".to_string());
        }

        let MctsCheckpointV3 {
            config,
            rng,
            nodes,
            candidates,
            seen,
        } = state;
        self.rng = rng;
        self.seed = config.seed;
        self.exploration = config.exploration;
        self.max_depth = config.max_depth;
        self.secondary_field = config.secondary_field;
        self.live_only = config.live_only;
        self.research_identity = config.research_identity;
        self.nodes = nodes;
        self.candidates = candidates;
        self.seen = seen;
        Ok(())
    }
}

impl MctsCheckpointV3 {
    fn validate(&self) -> Result<(), String> {
        if self.config.secondary_field.trim().is_empty()
            || !self.config.exploration.is_finite()
            || self.config.exploration < 0.0
            || self.config.max_depth == 0
        {
            return Err("invalid MCTS checkpoint configuration".to_string());
        }
        if let Some(identity) = &self.config.research_identity {
            identity.validate()?;
        }
        self.config
            .root_ast
            .validate()
            .map_err(|error| format!("invalid MCTS checkpoint root: {error}"))?;
        if self.config.live_only {
            validate_live_formula(&self.config.root_ast)
                .map_err(|error| format!("invalid live MCTS checkpoint root: {error}"))?;
        }
        let Some(root) = self.nodes.first() else {
            return Err("MCTS checkpoint has no root node".to_string());
        };
        if root.ast != self.config.root_ast || root.parent.is_some() || root.depth != 0 {
            return Err("MCTS checkpoint root does not match its configuration".to_string());
        }
        validate_tree(&self.nodes, 0, self.config.max_depth)
            .map_err(|error| format!("invalid MCTS checkpoint tree: {error}"))?;

        for (node_id, node) in self.nodes.iter().enumerate() {
            node.ast
                .validate()
                .map_err(|error| format!("invalid MCTS checkpoint node {node_id}: {error}"))?;
            if self.config.live_only {
                validate_live_formula(&node.ast).map_err(|error| {
                    format!("invalid live MCTS checkpoint node {node_id}: {error}")
                })?;
            }
            if node.depth == self.config.max_depth && !node.unexpanded_actions.is_empty() {
                return Err(format!("invalid MCTS checkpoint node {node_id}"));
            }
            let actions = node
                .unexpanded_actions
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            let permitted_actions = expansion_actions(self.config.live_only);
            if actions.len() != node.unexpanded_actions.len()
                || actions
                    .iter()
                    .any(|action| !permitted_actions.contains(action))
            {
                return Err(format!(
                    "invalid MCTS checkpoint actions for node {node_id}"
                ));
            }
        }

        let mut pending_nodes = BTreeSet::new();
        for (candidate_id, node_id) in &self.candidates {
            let Some(node) = self.nodes.get(*node_id) else {
                return Err("MCTS checkpoint candidate references an invalid node".to_string());
            };
            if candidate_id.is_empty()
                || *node_id == 0
                || !pending_nodes.insert(*node_id)
                || !self.seen.contains(&node.ast.to_string())
            {
                return Err("invalid MCTS checkpoint candidate map".to_string());
            }
        }
        if self.seen.iter().any(|formula| formula.is_empty()) {
            return Err("invalid MCTS checkpoint seen set".to_string());
        }
        Ok(())
    }
}

fn constant(value: &str) -> FactorAst {
    FactorAst::Terminal(FactorTerminal::Constant(value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> RemainingBudget {
        RemainingBudget {
            candidates: 16,
            expansions: 256,
            tokens: 0,
            milliseconds: 0,
        }
    }

    fn evaluation(score: f64) -> CandidateEvaluation {
        CandidateEvaluation {
            passed: true,
            score,
            failure_reasons: vec![],
            evaluator_version: "test".to_string(),
            evaluator_config: serde_json::json!({"fixture": true}),
            evaluation_protocol: None,
            evaluation_protocol_hash: None,
            metrics: crate::EvaluationMetrics {
                predictive: crate::PredictiveMetrics::from_folds(vec![]),
                row_count: 1,
                trade_count: 1,
                total_turnover: 1.0,
                mean_net_return: score,
                cumulative_net_return: score,
                max_drawdown: 0.0,
                net_sharpe: score,
                raw_score: score,
                adjusted_score: score,
                folds: vec![],
            },
        }
    }

    fn advance(engine: &mut MctsEngine, iteration: usize, score: f64) -> EngineProposal {
        let dataset = super::super::test_dataset();
        let proposal = engine
            .propose("mission", iteration, &dataset.proposal_context(), &budget())
            .unwrap();
        engine.observe(&proposal, &evaluation(score));
        proposal
    }

    #[test]
    fn live_mcts_only_emits_live_formulas() {
        let mut engine = MctsEngine::new_live(7, "best_bid", "best_ask", 1.4, 3).unwrap();
        let dataset = super::super::test_dataset();

        for iteration in 0..3 {
            let proposal = engine
                .propose("mission", iteration, &dataset.proposal_context(), &budget())
                .unwrap();
            let CandidateArtifact::Formula(ast) = &proposal.artifact else {
                panic!("live MCTS must produce a formula");
            };
            validate_live_formula(ast).expect("live MCTS formula must pass the shared gate");
            engine.abandon(&proposal);
        }
    }

    #[test]
    fn live_mcts_rejects_non_live_or_mixed_domain_fields() {
        assert!(MctsEngine::new_live(7, "signal", "best_bid", 1.4, 3).is_err());
        assert!(MctsEngine::new_live(7, "best_bid", "bar_return", 1.4, 3).is_err());
    }

    #[test]
    fn backpropagation_updates_root_and_child() {
        let mut engine = MctsEngine::new(3, "oi", "imbalance", 1.4, 3).unwrap();
        let child = engine.expand(0).unwrap();
        let proposal = EngineProposal {
            candidate_id: "candidate-1".to_string(),
            hypothesis: "test".to_string(),
            artifact: CandidateArtifact::Formula(engine.nodes[child].ast.clone()),
            expansions: 1,
            tokens: 0,
            elapsed_ms: 0,
        };
        engine
            .candidates
            .insert(proposal.candidate_id.clone(), child);
        engine.observe(&proposal, &evaluation(0.5));
        assert_eq!(engine.trace()[0].visits, 1);
        assert_eq!(engine.trace()[child].total_reward, 0.5);
    }

    #[test]
    fn checkpoint_round_trip_restores_complete_search_state() {
        let mut engine = MctsEngine::new(3, "oi", "imbalance", 1.4, 3).unwrap();
        advance(&mut engine, 0, 0.5);
        let dataset = super::super::test_dataset();
        let pending = engine
            .propose("mission", 1, &dataset.proposal_context(), &budget())
            .unwrap();
        let checkpoint = engine.checkpoint().unwrap();

        let mut restored = MctsEngine::new(3, "oi", "imbalance", 1.4, 3).unwrap();
        restored.restore_checkpoint(&checkpoint, &[]).unwrap();

        assert_eq!(restored.checkpoint().unwrap(), checkpoint);
        assert_eq!(restored.rng, engine.rng);
        assert_eq!(restored.seed, engine.seed);
        assert_eq!(restored.nodes, engine.nodes);
        assert_eq!(restored.exploration, engine.exploration);
        assert_eq!(restored.max_depth, engine.max_depth);
        assert_eq!(restored.secondary_field, engine.secondary_field);
        assert_eq!(restored.trace(), engine.trace());
        assert_eq!(restored.candidates, engine.candidates);
        assert_eq!(restored.seen, engine.seen);
        assert!(restored.candidates.contains_key(&pending.candidate_id));
    }

    #[test]
    fn shared_kernel_preserves_seeded_expansion_and_checkpoint_wire_shape() {
        let mut engine = MctsEngine::new(3, "oi", "imbalance", 1.4, 3).unwrap();
        let dataset = super::super::test_dataset();
        let first = engine
            .propose("mission", 0, &dataset.proposal_context(), &budget())
            .unwrap();
        let CandidateArtifact::Formula(first_ast) = &first.artifact else {
            panic!("MCTS must emit a formula candidate");
        };
        assert_eq!(first_ast.to_string(), "mean(oi, 20)");
        engine.observe(&first, &evaluation(0.5));

        let checkpoint = engine.checkpoint().unwrap();
        assert_eq!(
            checkpoint.state,
            serde_json::json!({
                "config": {
                    "seed": 3,
                    "root_ast": {"Terminal": {"Field": "oi"}},
                    "secondary_field": "imbalance",
                    "exploration": 1.4,
                    "max_depth": 3,
                    "live_only": false
                },
                "rng": 2_088_359_638_719_790_806_u64,
                "nodes": [
                    {
                        "ast": {"Terminal": {"Field": "oi"}},
                        "parent": null,
                        "children": [1],
                        "unexpanded_actions": [0, 1, 3],
                        "depth": 0,
                        "visits": 1,
                        "total_reward": 0.5,
                        "best_reward": 0.5
                    },
                    {
                        "ast": {"Call": {
                            "operator": "Mean",
                            "args": [
                                {"Terminal": {"Field": "oi"}},
                                {"Terminal": {"Constant": "20"}}
                            ]
                        }},
                        "parent": 0,
                        "children": [],
                        "unexpanded_actions": [0, 1, 2, 3],
                        "depth": 1,
                        "visits": 1,
                        "total_reward": 0.5,
                        "best_reward": 0.5
                    }
                ],
                "candidates": {},
                "seen": ["mean(oi, 20)"]
            })
        );
        let expected_trace = vec![
            MctsNodeSnapshot {
                node_id: 0,
                parent_id: None,
                visits: 1,
                total_reward: 0.5,
                best_reward: Some(0.5),
                formula: "oi".to_string(),
            },
            MctsNodeSnapshot {
                node_id: 1,
                parent_id: Some(0),
                visits: 1,
                total_reward: 0.5,
                best_reward: Some(0.5),
                formula: "mean(oi, 20)".to_string(),
            },
        ];
        assert_eq!(engine.trace(), expected_trace);

        let mut restored = MctsEngine::new(3, "oi", "imbalance", 1.4, 3).unwrap();
        restored.restore_checkpoint(&checkpoint, &[]).unwrap();
        assert_eq!(restored.trace(), expected_trace);
        let expected = engine
            .propose("mission", 1, &dataset.proposal_context(), &budget())
            .unwrap();
        let actual = restored
            .propose("mission", 1, &dataset.proposal_context(), &budget())
            .unwrap();
        assert_eq!(actual, expected);
        let CandidateArtifact::Formula(actual_ast) = &actual.artifact else {
            panic!("MCTS must emit a formula candidate");
        };
        assert_eq!(actual_ast.to_string(), "rank(oi)");
    }

    #[test]
    fn restored_search_continues_like_uninterrupted_search() {
        let mut uninterrupted = MctsEngine::new(9, "oi", "imbalance", 1.4, 4).unwrap();
        for (iteration, score) in [0.2, 0.8, -0.1].into_iter().enumerate() {
            advance(&mut uninterrupted, iteration, score);
        }
        let checkpoint = uninterrupted.checkpoint().unwrap();
        let mut restored = MctsEngine::new(9, "oi", "imbalance", 1.4, 4).unwrap();
        restored.restore_checkpoint(&checkpoint, &[]).unwrap();
        let dataset = super::super::test_dataset();

        let expected = uninterrupted
            .propose("mission", 3, &dataset.proposal_context(), &budget())
            .unwrap();
        let actual = restored
            .propose("mission", 3, &dataset.proposal_context(), &budget())
            .unwrap();

        assert_eq!(actual, expected);
        assert_eq!(
            restored.checkpoint().unwrap(),
            uninterrupted.checkpoint().unwrap()
        );
    }

    #[test]
    fn restore_checkpoint_fails_closed_on_config_or_state_mismatch() {
        let engine = MctsEngine::new(3, "oi", "imbalance", 1.4, 3).unwrap();
        let checkpoint = engine.checkpoint().unwrap();
        let mut wrong_kind = checkpoint.clone();
        wrong_kind.kind = EngineKind::BayesianOptimizer;
        let mut restored = MctsEngine::new(3, "oi", "imbalance", 1.4, 3).unwrap();
        assert!(restored.restore_checkpoint(&wrong_kind, &[]).is_err());

        let mut wrong_version = checkpoint.clone();
        wrong_version.version += 1;
        assert!(restored.restore_checkpoint(&wrong_version, &[]).is_err());

        let mut different_seed = MctsEngine::new(4, "oi", "imbalance", 1.4, 3).unwrap();
        assert!(different_seed.restore_checkpoint(&checkpoint, &[]).is_err());

        let mut different_config = MctsEngine::new(3, "price", "imbalance", 1.4, 3).unwrap();
        assert!(different_config
            .restore_checkpoint(&checkpoint, &[])
            .is_err());

        let mut malformed = checkpoint;
        malformed.state = serde_json::json!({"unexpected": true});
        assert!(restored.restore_checkpoint(&malformed, &[]).is_err());

        let mut cyclic = engine.checkpoint().unwrap();
        cyclic.state["nodes"][0]["children"] = serde_json::json!([0]);
        assert!(restored.restore_checkpoint(&cyclic, &[]).is_err());
    }

    #[test]
    fn heldout_mutation_cannot_change_real_mcts_candidate_or_backpropagation() {
        use crate::evaluation::{prepare_dataset, PreparedDataset, ResearchRow};
        use alpha_domain::{
            EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationWalkForwardV1,
        };
        use chrono::{Duration, Utc};

        fn dataset(mutate_holdout: bool) -> PreparedDataset {
            let start = chrono::DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
            let rows = (0..4)
                .map(|index| {
                    let signal = if mutate_holdout && index == 3 {
                        -1_000.0
                    } else {
                        index as f64
                    };
                    ResearchRow {
                        available_time: start + Duration::seconds(index),
                        signal,
                        features: BTreeMap::new(),
                        label: signal,
                        fee_bps: 0.0,
                        funding_bps: 0.0,
                        latency_bps: 0.0,
                    }
                })
                .collect();
            prepare_dataset(
                rows,
                &EvaluationProtocolV1::new(
                    EvaluationWalkForwardV1 {
                        initial_train_rows: 1,
                        validation_rows: 1,
                        fold_count: 1,
                        purge_rows: 1,
                        embargo_rows: 0,
                        sealed_holdout_rows: 1,
                    },
                    EvaluationCostsV1 {
                        fee_bps: 0.0,
                        rebate_bps: 0.0,
                        funding_bps: 0.0,
                        latency_bps: 0.0,
                        slippage_bps: 0.0,
                        cross_spread: false,
                        position_notional_usd: 0.0,
                        capacity_depth_levels: 0,
                        max_book_depth_fraction: 0.0,
                    },
                    EvaluationLabelSpecV1 {
                        horizon_buckets: 1,
                        observation_frequency_millis: 1_000,
                    },
                )
                .unwrap(),
            )
            .unwrap()
        }

        fn run(dataset: &PreparedDataset) -> (EngineProposal, ProposalEngineCheckpoint) {
            let mut engine = MctsEngine::new(7, "signal", "signal", 1.414, 5).unwrap();
            let proposal = engine
                .propose("mission-1", 1, &dataset.proposal_context(), &budget())
                .unwrap();
            engine.observe(&proposal, &evaluation(0.5));
            (proposal, engine.checkpoint().unwrap())
        }

        assert_eq!(run(&dataset(false)), run(&dataset(true)));
    }

    #[test]
    fn bound_checkpoint_rejects_snapshot_or_search_identity_drift() {
        fn identity(dataset: &str, max_candidates: usize) -> CexMctsSearchIdentityV1 {
            let now = chrono::Utc::now();
            let mission = alpha_domain::ResearchMission {
                mission_id: "mission-1".to_string(),
                objective: "test".to_string(),
                hypothesis_scope: "test".to_string(),
                mutable_scope: vec!["factor_ast".to_string()],
                dataset_manifest_id: hft_research_manifest::ManifestId::new(dataset).unwrap(),
                baseline_artifact_id: None,
                validation_mode: alpha_domain::ValidatorMode::MissionValidator,
                validator_spec: serde_json::json!({}),
                search_budget: alpha_domain::SearchBudget {
                    max_candidates,
                    max_expansions: 32,
                    max_tokens: 0,
                    max_seconds: 60,
                },
                completion_policy: alpha_domain::MissionCompletionPolicy::default(),
                prompt_snapshot_id: None,
                search_policy_snapshot_id: "mcts-lob-pit-v1".to_string(),
                status: alpha_domain::MissionStatus::Pending,
                terminal_reason: None,
                created_at: now,
                updated_at: now,
            };
            CexMctsSearchIdentityV1::for_mission(&mission, "a".repeat(64), "b".repeat(64), Some(1))
                .unwrap()
        }

        let engine =
            MctsEngine::new_live_bound(3, "best_bid", "best_ask", 1.4, 3, identity("dataset-a", 4))
                .unwrap();
        let checkpoint = engine.checkpoint().unwrap();
        assert_eq!(checkpoint.version, 3);

        let mut restored =
            MctsEngine::new_live_bound(3, "best_bid", "best_ask", 1.4, 3, identity("dataset-a", 4))
                .unwrap();
        restored.restore_checkpoint(&checkpoint, &[]).unwrap();
        assert_eq!(restored.checkpoint().unwrap(), checkpoint);

        for mismatched in [identity("dataset-b", 4), identity("dataset-a", 5)] {
            let mut restored =
                MctsEngine::new_live_bound(3, "best_bid", "best_ask", 1.4, 3, mismatched).unwrap();
            assert!(restored.restore_checkpoint(&checkpoint, &[]).is_err());
        }

        let mut protocol_drift = identity("dataset-a", 4);
        protocol_drift.evaluation_protocol_hash = "c".repeat(64);
        let mut restored =
            MctsEngine::new_live_bound(3, "best_bid", "best_ask", 1.4, 3, protocol_drift).unwrap();
        assert!(restored.restore_checkpoint(&checkpoint, &[]).is_err());

        let mut forged_tie_rule = checkpoint.clone();
        forged_tie_rule.state["config"]["research_identity"]["uct_tie_break_rule_id"] =
            serde_json::json!("first-child-on-tie");
        let mut restored =
            MctsEngine::new_live_bound(3, "best_bid", "best_ask", 1.4, 3, identity("dataset-a", 4))
                .unwrap();
        assert!(restored.restore_checkpoint(&forged_tie_rule, &[]).is_err());
    }

    #[test]
    fn abandon_removes_pending_candidate() {
        let mut engine = MctsEngine::new(3, "oi", "imbalance", 1.4, 3).unwrap();
        let dataset = super::super::test_dataset();
        let proposal = engine
            .propose("mission", 0, &dataset.proposal_context(), &budget())
            .unwrap();
        assert!(engine.candidates.contains_key(&proposal.candidate_id));

        engine.abandon(&proposal);

        assert!(!engine.candidates.contains_key(&proposal.candidate_id));
    }
}
