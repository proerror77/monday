use super::DeterministicRng;
use crate::{
    evaluation::EngineContext, CandidateEvaluation, EngineProposal, HistoricalObservation,
    ProposalEngine, ProposalEngineCheckpoint, RemainingBudget,
};
use alpha_domain::{CandidateArtifact, EngineKind};
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MCTS_CHECKPOINT_VERSION: u32 = 1;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MctsConfigV1 {
    seed: u64,
    root_ast: FactorAst,
    secondary_field: String,
    exploration: f64,
    max_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MctsCheckpointV1 {
    config: MctsConfigV1,
    rng: DeterministicRng,
    nodes: Vec<Node>,
    candidates: BTreeMap<String, usize>,
    seen: BTreeSet<String>,
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
            nodes: vec![Node {
                ast: FactorAst::Terminal(FactorTerminal::Field(root_field)),
                parent: None,
                children: vec![],
                unexpanded_actions: vec![0, 1, 2, 3],
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

    fn config(&self) -> Result<MctsConfigV1, String> {
        Ok(MctsConfigV1 {
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
        })
    }

    fn select_expandable(&self, node_id: usize) -> Option<usize> {
        let node = &self.nodes[node_id];
        if !node.unexpanded_actions.is_empty() {
            return Some(node_id);
        }
        node.children
            .iter()
            .copied()
            .filter(|child| self.has_expandable(*child))
            .max_by(|left, right| {
                self.uct(*left, node.visits)
                    .total_cmp(&self.uct(*right, node.visits))
            })
            .and_then(|child| self.select_expandable(child))
    }

    fn has_expandable(&self, node_id: usize) -> bool {
        !self.nodes[node_id].unexpanded_actions.is_empty()
            || self.nodes[node_id]
                .children
                .iter()
                .any(|child| self.has_expandable(*child))
    }

    fn uct(&self, node_id: usize, parent_visits: u64) -> f64 {
        let node = &self.nodes[node_id];
        if node.visits == 0 {
            return f64::INFINITY;
        }
        node.total_reward / node.visits as f64
            + self.exploration * ((parent_visits.max(1) as f64).ln() / node.visits as f64).sqrt()
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
                vec![0, 1, 2, 3]
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
        _context: &EngineContext<'_>,
        remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String> {
        if remaining.expansions == 0 {
            return Err("MCTS expansion budget is exhausted".to_string());
        }
        for attempt in 1..=remaining.expansions.min(256) {
            let parent = self
                .select_expandable(0)
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
        let Some(mut node_id) = self.candidates.remove(&proposal.candidate_id) else {
            return;
        };
        if !evaluation.score.is_finite() {
            return;
        }
        loop {
            let node = &mut self.nodes[node_id];
            node.visits += 1;
            node.total_reward += evaluation.score;
            node.best_reward = Some(
                node.best_reward
                    .map_or(evaluation.score, |best| best.max(evaluation.score)),
            );
            match node.parent {
                Some(parent) => node_id = parent,
                None => break,
            }
        }
    }

    fn abandon(&mut self, proposal: &EngineProposal) {
        self.candidates.remove(&proposal.candidate_id);
    }

    fn restore(&mut self, observations: &[HistoricalObservation]) -> Result<(), String> {
        for observation in observations {
            let CandidateArtifact::Formula(ast) = &observation.proposal.artifact else {
                return Err("MCTS history contains a non-formula artifact".to_string());
            };
            if !observation.evaluation.score.is_finite() {
                return Err("MCTS history contains a non-finite score".to_string());
            }
            self.seen.insert(ast.to_string());
            let root = &mut self.nodes[0];
            root.visits += 1;
            root.total_reward += observation.evaluation.score;
            root.best_reward = Some(
                root.best_reward
                    .map_or(observation.evaluation.score, |best| {
                        best.max(observation.evaluation.score)
                    }),
            );
        }
        Ok(())
    }

    fn checkpoint(&self) -> Result<ProposalEngineCheckpoint, String> {
        let state = MctsCheckpointV1 {
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
        let state: MctsCheckpointV1 = serde_json::from_value(checkpoint.state.clone())
            .map_err(|error| format!("invalid MCTS checkpoint state: {error}"))?;
        state.validate()?;
        if state.config != self.config()? {
            return Err("MCTS checkpoint configuration mismatch".to_string());
        }

        let MctsCheckpointV1 {
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
        self.nodes = nodes;
        self.candidates = candidates;
        self.seen = seen;
        Ok(())
    }
}

impl MctsCheckpointV1 {
    fn validate(&self) -> Result<(), String> {
        if self.config.secondary_field.trim().is_empty()
            || !self.config.exploration.is_finite()
            || self.config.exploration < 0.0
            || self.config.max_depth == 0
        {
            return Err("invalid MCTS checkpoint configuration".to_string());
        }
        self.config
            .root_ast
            .validate()
            .map_err(|error| format!("invalid MCTS checkpoint root: {error}"))?;
        let Some(root) = self.nodes.first() else {
            return Err("MCTS checkpoint has no root node".to_string());
        };
        if root.ast != self.config.root_ast || root.parent.is_some() || root.depth != 0 {
            return Err("MCTS checkpoint root does not match its configuration".to_string());
        }

        for (node_id, node) in self.nodes.iter().enumerate() {
            node.ast
                .validate()
                .map_err(|error| format!("invalid MCTS checkpoint node {node_id}: {error}"))?;
            if !node.total_reward.is_finite()
                || node.best_reward.is_some_and(|reward| !reward.is_finite())
                || node.depth > self.config.max_depth
                || (node.depth == self.config.max_depth && !node.unexpanded_actions.is_empty())
            {
                return Err(format!("invalid MCTS checkpoint node {node_id}"));
            }
            if (node.visits == 0) != (node.total_reward == 0.0 && node.best_reward.is_none()) {
                return Err(format!(
                    "invalid MCTS checkpoint rewards for node {node_id}"
                ));
            }
            let actions = node
                .unexpanded_actions
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            if actions.len() != node.unexpanded_actions.len()
                || actions.iter().any(|action| *action > 3)
            {
                return Err(format!(
                    "invalid MCTS checkpoint actions for node {node_id}"
                ));
            }
            if node.children.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(format!(
                    "invalid MCTS checkpoint children for node {node_id}"
                ));
            }
            for child_id in &node.children {
                let Some(child) = self.nodes.get(*child_id) else {
                    return Err(format!("invalid MCTS checkpoint child for node {node_id}"));
                };
                if *child_id <= node_id || child.parent != Some(node_id) {
                    return Err(format!("invalid MCTS checkpoint child for node {node_id}"));
                }
            }
            if node_id > 0 {
                let Some(parent_id) = node.parent else {
                    return Err(format!("MCTS checkpoint node {node_id} has no parent"));
                };
                let Some(parent) = self.nodes.get(parent_id) else {
                    return Err(format!("MCTS checkpoint node {node_id} has invalid parent"));
                };
                if parent_id >= node_id
                    || node.depth != parent.depth + 1
                    || !parent.children.contains(&node_id)
                {
                    return Err(format!("MCTS checkpoint node {node_id} is inconsistent"));
                }
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
        }
    }

    fn advance(engine: &mut MctsEngine, iteration: usize, score: f64) -> EngineProposal {
        let dataset = super::super::test_dataset();
        let proposal = engine
            .propose("mission", iteration, &dataset.engine_context(), &budget())
            .unwrap();
        engine.observe(&proposal, &evaluation(score));
        proposal
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
        engine.observe(
            &proposal,
            &CandidateEvaluation {
                passed: true,
                score: 0.5,
                failure_reasons: vec![],
                evaluator_version: "test".to_string(),
            },
        );
        assert_eq!(engine.trace()[0].visits, 1);
        assert_eq!(engine.trace()[child].total_reward, 0.5);
    }

    #[test]
    fn checkpoint_round_trip_restores_complete_search_state() {
        let mut engine = MctsEngine::new(3, "oi", "imbalance", 1.4, 3).unwrap();
        advance(&mut engine, 0, 0.5);
        let dataset = super::super::test_dataset();
        let pending = engine
            .propose("mission", 1, &dataset.engine_context(), &budget())
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
            .propose("mission", 3, &dataset.engine_context(), &budget())
            .unwrap();
        let actual = restored
            .propose("mission", 3, &dataset.engine_context(), &budget())
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
    }

    #[test]
    fn abandon_removes_pending_candidate() {
        let mut engine = MctsEngine::new(3, "oi", "imbalance", 1.4, 3).unwrap();
        let dataset = super::super::test_dataset();
        let proposal = engine
            .propose("mission", 0, &dataset.engine_context(), &budget())
            .unwrap();
        assert!(engine.candidates.contains_key(&proposal.candidate_id));

        engine.abandon(&proposal);

        assert!(!engine.candidates.contains_key(&proposal.candidate_id));
    }
}
