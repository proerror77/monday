//! Search proposal contracts for GP, QD, MCTS, RL, LLM, and Bayesian engines.

use chrono::{DateTime, Utc};
use hft_factor_dsl::FactorAst;
use hft_research_manifest::ManifestId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SearchProtocolError {
    #[error("proposal id cannot be empty")]
    EmptyProposalId,
    #[error("MCTS node {node_id} references itself as parent")]
    SelfParent { node_id: String },
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
    pub parameters: BTreeMap<String, String>,
    pub rationale: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ProposalArtifact {
    pub fn validate(&self) -> Result<(), SearchProtocolError> {
        if self.proposal_id.trim().is_empty() {
            return Err(SearchProtocolError::EmptyProposalId);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_proposal_id() {
        let ast = FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field("oi".to_string()));
        let artifact = ProposalArtifact {
            proposal_id: " ".to_string(),
            engine: SearchEngineKind::ManualSeed,
            search_manifest_id: ManifestId::new("search-1").unwrap(),
            parent_factor_ids: vec![],
            ast,
            parameters: BTreeMap::new(),
            rationale: None,
            created_at: Utc::now(),
        };
        assert_eq!(
            artifact.validate().unwrap_err(),
            SearchProtocolError::EmptyProposalId
        );
    }

    #[test]
    fn rejects_mcts_self_parent() {
        let node = MctsTraceNode {
            node_id: "n1".to_string(),
            parent_node_id: Some("n1".to_string()),
            visits: 1,
            total_reward: 0.1,
            best_reward: 0.1,
        };
        assert_eq!(
            node.validate().unwrap_err(),
            SearchProtocolError::SelfParent {
                node_id: "n1".to_string()
            }
        );
    }
}
