//! Search proposal contracts for GP, QD, MCTS, RL, LLM, and Bayesian engines.

use chrono::{DateTime, Utc};
use hft_factor_dsl::{FactorAst, FactorDslError};
use hft_research_manifest::ManifestId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SearchProtocolError {
    #[error("proposal id cannot be empty")]
    EmptyProposalId,
    #[error("invalid factor AST: {0}")]
    InvalidFactorAst(#[from] FactorDslError),
    #[error("MCTS proposals require an MCTS trace")]
    MissingMctsTrace,
    #[error("MCTS root node id cannot be empty")]
    EmptyMctsRoot,
    #[error("MCTS node id cannot be empty")]
    EmptyMctsNodeId,
    #[error("MCTS node id {node_id} is duplicated")]
    DuplicateMctsNode { node_id: String },
    #[error("MCTS root node {node_id} is missing")]
    MissingMctsRoot { node_id: String },
    #[error("MCTS node {node_id} references missing parent {parent_node_id}")]
    MissingMctsParent {
        node_id: String,
        parent_node_id: String,
    },
    #[error("MCTS node {node_id} references itself as parent")]
    SelfParent { node_id: String },
    #[error("MCTS parent chain for node {node_id} contains a cycle")]
    MctsCycle { node_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchEngineKind {
    GeneticProgramming,
    QualityDiversity,
    Mcts,
    ReinforcementLearning,
    LlmProposer,
    BayesianOptimizer,
    ManualSeed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalArtifact {
    pub proposal_id: String,
    pub engine: SearchEngineKind,
    pub search_manifest_id: ManifestId,
    pub parent_factor_ids: Vec<String>,
    pub ast: FactorAst,
    #[serde(default)]
    pub mcts_trace: Option<MctsTrace>,
    pub parameters: BTreeMap<String, String>,
    pub rationale: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ProposalArtifact {
    pub fn validate(&self) -> Result<(), SearchProtocolError> {
        if self.proposal_id.trim().is_empty() {
            return Err(SearchProtocolError::EmptyProposalId);
        }
        self.ast.validate()?;
        if self.engine == SearchEngineKind::Mcts && self.mcts_trace.is_none() {
            return Err(SearchProtocolError::MissingMctsTrace);
        }
        if let Some(trace) = &self.mcts_trace {
            trace.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MctsTraceNode {
    pub node_id: String,
    pub parent_node_id: Option<String>,
    pub visits: u64,
    pub total_reward: f64,
    pub best_reward: f64,
}

impl MctsTraceNode {
    pub fn validate(&self) -> Result<(), SearchProtocolError> {
        if self.node_id.trim().is_empty() {
            return Err(SearchProtocolError::EmptyMctsNodeId);
        }
        if self.parent_node_id.as_deref() == Some(self.node_id.as_str()) {
            return Err(SearchProtocolError::SelfParent {
                node_id: self.node_id.clone(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MctsTrace {
    pub root_node_id: String,
    pub nodes: Vec<MctsTraceNode>,
    pub backpropagation_truncated_count: u64,
}

impl MctsTrace {
    pub fn validate(&self) -> Result<(), SearchProtocolError> {
        if self.root_node_id.trim().is_empty() {
            return Err(SearchProtocolError::EmptyMctsRoot);
        }

        let mut node_parents = BTreeMap::new();
        for node in &self.nodes {
            node.validate()?;
            if node_parents
                .insert(node.node_id.as_str(), node.parent_node_id.as_deref())
                .is_some()
            {
                return Err(SearchProtocolError::DuplicateMctsNode {
                    node_id: node.node_id.clone(),
                });
            }
        }

        if !node_parents.contains_key(self.root_node_id.as_str()) {
            return Err(SearchProtocolError::MissingMctsRoot {
                node_id: self.root_node_id.clone(),
            });
        }

        for node in &self.nodes {
            let mut seen = BTreeSet::new();
            let mut current = Some(node.node_id.as_str());
            while let Some(node_id) = current {
                if !seen.insert(node_id) {
                    return Err(SearchProtocolError::MctsCycle {
                        node_id: node.node_id.clone(),
                    });
                }
                let parent = node_parents[node_id];
                if let Some(parent_node_id) = parent {
                    if !node_parents.contains_key(parent_node_id) {
                        return Err(SearchProtocolError::MissingMctsParent {
                            node_id: node_id.to_string(),
                            parent_node_id: parent_node_id.to_string(),
                        });
                    }
                }
                current = parent;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field_ast() -> FactorAst {
        FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field("oi".to_string()))
    }

    fn bad_ast() -> FactorAst {
        FactorAst::Call {
            operator: hft_factor_dsl::FactorOperator::Add,
            args: vec![field_ast()],
        }
    }

    fn node(node_id: &str, parent_node_id: Option<&str>) -> MctsTraceNode {
        MctsTraceNode {
            node_id: node_id.to_string(),
            parent_node_id: parent_node_id.map(str::to_string),
            visits: 1,
            total_reward: 0.1,
            best_reward: 0.1,
        }
    }

    fn trace(nodes: Vec<MctsTraceNode>) -> MctsTrace {
        MctsTrace {
            root_node_id: "root".to_string(),
            nodes,
            backpropagation_truncated_count: 0,
        }
    }

    fn proposal(ast: FactorAst, mcts_trace: Option<MctsTrace>) -> ProposalArtifact {
        ProposalArtifact {
            proposal_id: "proposal-1".to_string(),
            engine: SearchEngineKind::ManualSeed,
            search_manifest_id: ManifestId::new("search-1").unwrap(),
            parent_factor_ids: vec![],
            ast,
            mcts_trace,
            parameters: BTreeMap::new(),
            rationale: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn rejects_empty_proposal_id() {
        let mut artifact = proposal(field_ast(), None);
        artifact.proposal_id = " ".to_string();

        assert_eq!(
            artifact.validate().unwrap_err(),
            SearchProtocolError::EmptyProposalId
        );
    }

    #[test]
    fn rejects_bad_proposal_ast() {
        let artifact = proposal(bad_ast(), None);

        assert!(matches!(
            artifact.validate().unwrap_err(),
            SearchProtocolError::InvalidFactorAst(_)
        ));
    }

    #[test]
    fn rejects_bad_proposal_mcts_trace() {
        let artifact = proposal(field_ast(), Some(trace(vec![node("child", Some("missing"))])));

        assert_eq!(
            artifact.validate().unwrap_err(),
            SearchProtocolError::MissingMctsRoot {
                node_id: "root".to_string()
            }
        );
    }

    #[test]
    fn rejects_mcts_proposal_without_trace() {
        let mut artifact = proposal(field_ast(), None);
        artifact.engine = SearchEngineKind::Mcts;

        assert_eq!(
            artifact.validate().unwrap_err(),
            SearchProtocolError::MissingMctsTrace
        );
    }

    #[test]
    fn rejects_mcts_self_parent() {
        let node = node("n1", Some("n1"));

        assert_eq!(
            node.validate().unwrap_err(),
            SearchProtocolError::SelfParent {
                node_id: "n1".to_string()
            }
        );
    }

    #[test]
    fn validates_mcts_trace_graph() {
        let trace = trace(vec![node("root", None), node("child", Some("root"))]);

        assert_eq!(trace.validate(), Ok(()));
    }

    #[test]
    fn rejects_mcts_trace_cycles() {
        let trace = trace(vec![
            node("root", None),
            node("a", Some("b")),
            node("b", Some("a")),
        ]);

        assert_eq!(
            trace.validate().unwrap_err(),
            SearchProtocolError::MctsCycle {
                node_id: "a".to_string()
            }
        );
    }

    #[test]
    fn rejects_mcts_trace_duplicate_ids_and_missing_parents() {
        let duplicate = trace(vec![node("root", None), node("root", None)]);
        assert_eq!(
            duplicate.validate().unwrap_err(),
            SearchProtocolError::DuplicateMctsNode {
                node_id: "root".to_string()
            }
        );

        let missing_parent = trace(vec![node("root", None), node("child", Some("missing"))]);
        assert_eq!(
            missing_parent.validate().unwrap_err(),
            SearchProtocolError::MissingMctsParent {
                node_id: "child".to_string(),
                parent_node_id: "missing".to_string()
            }
        );
    }
}
