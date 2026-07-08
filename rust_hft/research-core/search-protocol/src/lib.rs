//! Search proposal contracts for GP, QD, MCTS, RL, LLM, and Bayesian engines.

use chrono::{DateTime, Utc};
use hft_factor_dsl::{FactorAst, FactorDslError, FactorOperator, FactorTerminal};
use hft_research_manifest::{ManifestError, ManifestId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SearchProtocolError {
    #[error("proposal id cannot be empty")]
    EmptyProposalId,
    #[error("search run id cannot be empty")]
    EmptyRunId,
    #[error("invalid factor AST: {0}")]
    InvalidFactorAst(#[from] FactorDslError),
    #[error("invalid search manifest: {0}")]
    InvalidSearchManifest(#[from] ManifestError),
    #[error("search budget must request at least one candidate")]
    InvalidCandidateBudget,
    #[error("search budget must set at least one positive limit")]
    EmptyBudgetLimit,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchBudget {
    pub max_candidates: usize,
    pub max_expansions: u64,
    pub max_tokens: u64,
    pub max_seconds: u64,
}

impl SearchBudget {
    pub fn validate(&self) -> Result<(), SearchProtocolError> {
        if self.max_candidates == 0 {
            return Err(SearchProtocolError::InvalidCandidateBudget);
        }
        if self.max_expansions == 0 && self.max_tokens == 0 && self.max_seconds == 0 {
            return Err(SearchProtocolError::EmptyBudgetLimit);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRunRequest {
    pub run_id: String,
    pub engine: SearchEngineKind,
    pub search_manifest_id: ManifestId,
    pub budget: SearchBudget,
}

impl SearchRunRequest {
    pub fn validate(&self) -> Result<(), SearchProtocolError> {
        if self.run_id.trim().is_empty() {
            return Err(SearchProtocolError::EmptyRunId);
        }
        self.search_manifest_id.validate()?;
        self.budget.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchRunReport {
    pub request: SearchRunRequest,
    pub proposals: Vec<ProposalArtifact>,
}

pub fn run_budgeted_lab_search(
    request: SearchRunRequest,
    ast: FactorAst,
    created_at: DateTime<Utc>,
) -> Result<SearchRunReport, SearchProtocolError> {
    request.validate()?;
    ast.validate()?;
    let proposal_count = request.budget.max_candidates;
    let proposals = (0..proposal_count)
        .map(|idx| {
            let mcts_trace = (request.engine == SearchEngineKind::Mcts).then(|| MctsTrace {
                root_node_id: "root".to_string(),
                nodes: vec![
                    MctsTraceNode {
                        node_id: "root".to_string(),
                        parent_node_id: None,
                        visits: request.budget.max_expansions.max(1),
                        total_reward: 0.0,
                        best_reward: 0.0,
                    },
                    MctsTraceNode {
                        node_id: format!("candidate-{}", idx + 1),
                        parent_node_id: Some("root".to_string()),
                        visits: 1,
                        total_reward: 0.1 + idx as f64 * 0.01,
                        best_reward: 0.1 + idx as f64 * 0.01,
                    },
                ],
                backpropagation_truncated_count: 0,
            });
            let candidate_ast = mutate_ast(&request.engine, &ast, idx)?;
            let artifact = ProposalArtifact {
                proposal_id: format!("{}-proposal-{}", request.run_id, idx + 1),
                engine: request.engine.clone(),
                search_manifest_id: request.search_manifest_id.clone(),
                parent_factor_ids: vec![],
                ast: candidate_ast,
                mcts_trace,
                parameters: BTreeMap::from([
                    ("candidate_index".to_string(), (idx + 1).to_string()),
                    (
                        "max_expansions".to_string(),
                        request.budget.max_expansions.to_string(),
                    ),
                    (
                        "max_tokens".to_string(),
                        request.budget.max_tokens.to_string(),
                    ),
                    (
                        "max_seconds".to_string(),
                        request.budget.max_seconds.to_string(),
                    ),
                ]),
                rationale: Some(
                    match request.engine {
                        SearchEngineKind::Mcts => {
                            "MCTS expanded a candidate from the root search node"
                        }
                        SearchEngineKind::ReinforcementLearning => {
                            "RL policy sampled a candidate action from the lab budget"
                        }
                        SearchEngineKind::LlmProposer => {
                            "LLM proposer drafted a rule candidate from local prompt priors"
                        }
                        _ => "budgeted lab search proposal",
                    }
                    .to_string(),
                ),
                created_at,
            };
            artifact.validate()?;
            Ok(artifact)
        })
        .collect::<Result<Vec<_>, SearchProtocolError>>()?;

    Ok(SearchRunReport { request, proposals })
}

fn mutate_ast(
    engine: &SearchEngineKind,
    ast: &FactorAst,
    idx: usize,
) -> Result<FactorAst, SearchProtocolError> {
    let field = |name: &str| FactorAst::Terminal(FactorTerminal::Field(name.to_string()));
    let constant = |value: &str| FactorAst::Terminal(FactorTerminal::Constant(value.to_string()));
    let candidate = match engine {
        SearchEngineKind::Mcts => match idx % 3 {
            0 => FactorAst::call(FactorOperator::Rank, vec![ast.clone()])?,
            1 => FactorAst::call(FactorOperator::Delta, vec![ast.clone(), constant("5")])?,
            _ => FactorAst::call(FactorOperator::Mean, vec![ast.clone(), constant("20")])?,
        },
        SearchEngineKind::ReinforcementLearning => match idx % 3 {
            0 => FactorAst::call(FactorOperator::Mul, vec![ast.clone(), field("spread_bps")])?,
            1 => FactorAst::call(
                FactorOperator::Sub,
                vec![ast.clone(), field("funding_rate")],
            )?,
            _ => FactorAst::call(FactorOperator::ZScore, vec![ast.clone(), constant("60")])?,
        },
        SearchEngineKind::LlmProposer => match idx % 3 {
            0 => FactorAst::call(
                FactorOperator::Add,
                vec![ast.clone(), field("cvd_slope_5m")],
            )?,
            1 => FactorAst::call(
                FactorOperator::Div,
                vec![ast.clone(), field("depth_imbalance")],
            )?,
            _ => FactorAst::call(FactorOperator::Abs, vec![ast.clone()])?,
        },
        _ => ast.clone(),
    };
    Ok(candidate)
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
        let artifact = proposal(
            field_ast(),
            Some(trace(vec![node("child", Some("missing"))])),
        );

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
    fn budgeted_mcts_run_includes_trace() {
        let report = run_budgeted_lab_search(
            SearchRunRequest {
                run_id: "mcts-run".to_string(),
                engine: SearchEngineKind::Mcts,
                search_manifest_id: ManifestId::new("search-1").unwrap(),
                budget: SearchBudget {
                    max_candidates: 1,
                    max_expansions: 8,
                    max_tokens: 0,
                    max_seconds: 1,
                },
            },
            field_ast(),
            Utc::now(),
        )
        .unwrap();

        assert_eq!(report.proposals.len(), 1);
        assert!(report.proposals[0].mcts_trace.is_some());
    }

    #[test]
    fn budgeted_run_generates_multiple_distinct_candidates() {
        let report = run_budgeted_lab_search(
            SearchRunRequest {
                run_id: "rl-run".to_string(),
                engine: SearchEngineKind::ReinforcementLearning,
                search_manifest_id: ManifestId::new("search-1").unwrap(),
                budget: SearchBudget {
                    max_candidates: 3,
                    max_expansions: 8,
                    max_tokens: 0,
                    max_seconds: 1,
                },
            },
            field_ast(),
            Utc::now(),
        )
        .unwrap();

        assert_eq!(report.proposals.len(), 3);
        assert_ne!(report.proposals[0].ast, report.proposals[1].ast);
        assert_eq!(
            report.proposals[2].parameters["candidate_index"],
            "3".to_string()
        );
    }

    #[test]
    fn rejects_empty_budget_limit() {
        assert_eq!(
            SearchBudget {
                max_candidates: 1,
                max_expansions: 0,
                max_tokens: 0,
                max_seconds: 0,
            }
            .validate()
            .unwrap_err(),
            SearchProtocolError::EmptyBudgetLimit
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
