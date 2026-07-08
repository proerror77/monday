//! Contracts for wrapping existing research prototypes without granting runtime authority.

use chrono::{DateTime, Utc};
use hft_factor_dsl::FactorAst;
use hft_research_manifest::ManifestId;
use hft_search_protocol::{ProposalArtifact, SearchEngineKind, SearchProtocolError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PrototypeAdapterError {
    #[error("prototype backend id cannot be empty")]
    EmptyBackendId,
    #[error("prototype source path cannot be empty")]
    EmptySourcePath,
    #[error("prototype adapter cannot grant live trading authority")]
    LiveTradingAuthorityDenied,
    #[error("prototype proposal is invalid: {0}")]
    InvalidProposal(#[from] SearchProtocolError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrototypeBackendKind {
    LobAlphaSearch,
    OhlcvAlphaGenerator,
    BboOptimizer,
    RlLabGenerator,
    SignalAggregator,
    SmartExitManager,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrototypeRights {
    pub may_generate_proposals: bool,
    pub may_write_factor_bank: bool,
    pub may_live_trade: bool,
}

impl PrototypeRights {
    pub fn lab_only() -> Self {
        Self {
            may_generate_proposals: true,
            may_write_factor_bank: false,
            may_live_trade: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrototypeBackend {
    pub backend_id: String,
    pub kind: PrototypeBackendKind,
    pub source_path: String,
    pub engine: SearchEngineKind,
    pub rights: PrototypeRights,
}

impl PrototypeBackend {
    pub fn validate(&self) -> Result<(), PrototypeAdapterError> {
        if self.backend_id.trim().is_empty() {
            return Err(PrototypeAdapterError::EmptyBackendId);
        }
        if self.source_path.trim().is_empty() {
            return Err(PrototypeAdapterError::EmptySourcePath);
        }
        if self.rights.may_live_trade {
            return Err(PrototypeAdapterError::LiveTradingAuthorityDenied);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrototypeRunRequest {
    pub search_manifest_id: ManifestId,
    pub max_candidates: usize,
}

pub trait PrototypeProposalAdapter {
    fn backend(&self) -> &PrototypeBackend;
    fn propose(
        &self,
        request: &PrototypeRunRequest,
        created_at: DateTime<Utc>,
    ) -> Result<Vec<ProposalArtifact>, PrototypeAdapterError>;
}

#[derive(Debug, Clone)]
pub struct StaticPrototypeAdapter {
    backend: PrototypeBackend,
    ast: FactorAst,
}

impl StaticPrototypeAdapter {
    pub fn new(backend: PrototypeBackend, ast: FactorAst) -> Result<Self, PrototypeAdapterError> {
        backend.validate()?;
        Ok(Self { backend, ast })
    }
}

impl PrototypeProposalAdapter for StaticPrototypeAdapter {
    fn backend(&self) -> &PrototypeBackend {
        &self.backend
    }

    fn propose(
        &self,
        request: &PrototypeRunRequest,
        created_at: DateTime<Utc>,
    ) -> Result<Vec<ProposalArtifact>, PrototypeAdapterError> {
        let limit = request.max_candidates.min(1);
        if limit == 0 || !self.backend.rights.may_generate_proposals {
            return Ok(vec![]);
        }
        let artifact = ProposalArtifact {
            proposal_id: format!("{}-proposal-1", self.backend.backend_id),
            engine: self.backend.engine.clone(),
            search_manifest_id: request.search_manifest_id.clone(),
            parent_factor_ids: vec![],
            ast: self.ast.clone(),
            mcts_trace: None,
            parameters: BTreeMap::from([(
                "source_path".to_string(),
                self.backend.source_path.clone(),
            )]),
            rationale: Some(format!("prototype backend {}", self.backend.backend_id)),
            created_at,
        };
        artifact.validate()?;
        Ok(vec![artifact])
    }
}

pub fn known_python_prototypes() -> Vec<PrototypeBackend> {
    vec![
        PrototypeBackend {
            backend_id: "lob-alpha-search".to_string(),
            kind: PrototypeBackendKind::LobAlphaSearch,
            source_path: "ml_workspace/lob_core/alpha_search.py".to_string(),
            engine: SearchEngineKind::GeneticProgramming,
            rights: PrototypeRights::lab_only(),
        },
        PrototypeBackend {
            backend_id: "ohlcv-alpha-generator".to_string(),
            kind: PrototypeBackendKind::OhlcvAlphaGenerator,
            source_path: "ml_workspace/algorithms/alpha/true_alpha_generator.py".to_string(),
            engine: SearchEngineKind::GeneticProgramming,
            rights: PrototypeRights::lab_only(),
        },
        PrototypeBackend {
            backend_id: "bbo-search".to_string(),
            kind: PrototypeBackendKind::BboOptimizer,
            source_path: "ml_workspace/algorithms/bbo/search.py".to_string(),
            engine: SearchEngineKind::BayesianOptimizer,
            rights: PrototypeRights::lab_only(),
        },
        PrototypeBackend {
            backend_id: "rl-lab-generator".to_string(),
            kind: PrototypeBackendKind::RlLabGenerator,
            source_path: "ml_workspace/algorithms/rl/trainer.py".to_string(),
            engine: SearchEngineKind::ReinforcementLearning,
            rights: PrototypeRights::lab_only(),
        },
        PrototypeBackend {
            backend_id: "signal-aggregator".to_string(),
            kind: PrototypeBackendKind::SignalAggregator,
            source_path: "ml_workspace/algorithms/signal_aggregator.py".to_string(),
            engine: SearchEngineKind::QualityDiversity,
            rights: PrototypeRights::lab_only(),
        },
        PrototypeBackend {
            backend_id: "smart-exit-manager".to_string(),
            kind: PrototypeBackendKind::SmartExitManager,
            source_path: "ml_workspace/algorithms/smart_exit_manager.py".to_string(),
            engine: SearchEngineKind::ReinforcementLearning,
            rights: PrototypeRights::lab_only(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_factor_dsl::{FactorAst, FactorTerminal};

    #[test]
    fn rejects_live_trading_authority() {
        let mut backend = known_python_prototypes().remove(0);
        backend.rights.may_live_trade = true;

        assert_eq!(
            backend.validate().unwrap_err(),
            PrototypeAdapterError::LiveTradingAuthorityDenied
        );
    }

    #[test]
    fn static_adapter_generates_valid_lab_proposal() {
        let backend = known_python_prototypes().remove(0);
        let adapter = StaticPrototypeAdapter::new(
            backend,
            FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string())),
        )
        .unwrap();

        let proposals = adapter
            .propose(
                &PrototypeRunRequest {
                    search_manifest_id: ManifestId::new("search-1").unwrap(),
                    max_candidates: 1,
                },
                Utc::now(),
            )
            .unwrap();

        assert_eq!(proposals.len(), 1);
        assert_eq!(
            proposals[0].parameters["source_path"],
            "ml_workspace/lob_core/alpha_search.py"
        );
    }

    #[test]
    fn known_python_prototypes_are_lab_only() {
        let prototypes = known_python_prototypes();

        assert!(prototypes
            .iter()
            .any(|backend| backend.kind == PrototypeBackendKind::RlLabGenerator));
        assert!(prototypes
            .iter()
            .any(|backend| backend.kind == PrototypeBackendKind::SignalAggregator));
        assert!(prototypes
            .iter()
            .any(|backend| backend.kind == PrototypeBackendKind::SmartExitManager));
        assert!(prototypes.iter().all(|backend| {
            backend.rights.may_generate_proposals
                && !backend.rights.may_write_factor_bank
                && !backend.rights.may_live_trade
        }));
    }
}
