use super::DeterministicRng;
use crate::{
    evaluation::EngineContext, CandidateEvaluation, EngineProposal, ProposalEngine, RemainingBudget,
};
use alpha_domain::{CandidateArtifact, EngineKind};
use hft_factor_dsl::{FactorAst, FactorOperator, FactorTerminal};
use std::collections::BTreeMap;

struct Node {
    ast: FactorAst,
    parent: Option<usize>,
    children: Vec<usize>,
    unexpanded_actions: Vec<usize>,
    depth: usize,
    visits: u64,
    total_reward: f64,
    best_reward: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MctsNodeSnapshot {
    pub node_id: usize,
    pub parent_id: Option<usize>,
    pub visits: u64,
    pub total_reward: f64,
    pub best_reward: f64,
    pub formula: String,
}

pub struct MctsEngine {
    rng: DeterministicRng,
    exploration: f64,
    max_depth: usize,
    secondary_field: String,
    nodes: Vec<Node>,
    candidates: BTreeMap<String, usize>,
}

impl MctsEngine {
    pub fn new(
        seed: u64,
        root_field: impl Into<String>,
        secondary_field: impl Into<String>,
        exploration: f64,
        max_depth: usize,
    ) -> Result<Self, String> {
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
                best_reward: f64::NEG_INFINITY,
            }],
            candidates: BTreeMap::new(),
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
            best_reward: f64::NEG_INFINITY,
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
        let parent = self
            .select_expandable(0)
            .ok_or_else(|| "MCTS tree has no expandable node".to_string())?;
        let node_id = self.expand(parent)?;
        let candidate_id = format!("{mission_id}-mcts-{iteration_index}");
        self.candidates.insert(candidate_id.clone(), node_id);
        Ok(EngineProposal {
            candidate_id,
            hypothesis: "MCTS grammar expansion selected by UCT".to_string(),
            artifact: CandidateArtifact::Formula(self.nodes[node_id].ast.clone()),
            expansions: 1,
            tokens: 0,
            elapsed_ms: 0,
        })
    }

    fn observe(&mut self, proposal: &EngineProposal, evaluation: &CandidateEvaluation) {
        let Some(mut node_id) = self.candidates.remove(&proposal.candidate_id) else {
            return;
        };
        loop {
            let node = &mut self.nodes[node_id];
            node.visits += 1;
            node.total_reward += evaluation.score;
            node.best_reward = node.best_reward.max(evaluation.score);
            match node.parent {
                Some(parent) => node_id = parent,
                None => break,
            }
        }
    }
}

fn constant(value: &str) -> FactorAst {
    FactorAst::Terminal(FactorTerminal::Constant(value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
