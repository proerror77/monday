//! Factor Bank contracts for auditable alpha assets.

use chrono::{DateTime, Utc};
use hft_factor_dsl::{FactorAst, FactorDslError};
use hft_research_manifest::{ManifestId, ManifestRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FactorBankError {
    #[error("factor id cannot be empty")]
    EmptyFactorId,
    #[error("invalid factor AST: {0}")]
    InvalidFactorAst(#[from] FactorDslError),
    #[error("live full candidate is bookkeeping only in MVP")]
    LiveFullCandidateNotExecutable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorType {
    Formula,
    Program,
    ModelFeature,
    Model,
    Ensemble,
    AllocatorPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorStatus {
    Generated,
    QuickTestPassed,
    FullBacktestPassed,
    PaperTrading,
    LiveShadow,
    LiveSmallPendingApproval,
    LiveSmall,
    LiveFullCandidate,
    Decayed,
    Retired,
    Rejected,
}

impl FactorStatus {
    pub fn executable_in_mvp(&self) -> bool {
        matches!(self, Self::PaperTrading | Self::LiveShadow | Self::LiveSmall)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactorMetrics {
    pub rank_ic: Option<f64>,
    pub icir: Option<f64>,
    pub net_sharpe: Option<f64>,
    pub max_drawdown: Option<f64>,
    pub turnover: Option<f64>,
    pub custom: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactorLineage {
    pub parent_factor_ids: Vec<String>,
    pub source_engine: String,
    pub search_manifest_id: ManifestId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactorAsset {
    pub factor_id: String,
    pub factor_type: FactorType,
    pub ast: FactorAst,
    pub lineage: FactorLineage,
    pub data_manifest: ManifestRef,
    pub feature_manifest: ManifestRef,
    pub label_manifest: ManifestRef,
    pub evaluation_manifests: Vec<ManifestRef>,
    pub metrics: FactorMetrics,
    pub correlation_cluster: Option<String>,
    pub regime_metrics: BTreeMap<String, FactorMetrics>,
    pub symbol_metrics: BTreeMap<String, FactorMetrics>,
    pub promotion_status: FactorStatus,
    pub live_decay_state: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl FactorAsset {
    pub fn validate(&self) -> Result<(), FactorBankError> {
        if self.factor_id.trim().is_empty() {
            return Err(FactorBankError::EmptyFactorId);
        }
        self.ast.validate()?;
        if self.promotion_status == FactorStatus::LiveFullCandidate {
            return Err(FactorBankError::LiveFullCandidateNotExecutable);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_factor_dsl::FactorTerminal;
    use hft_research_manifest::{ManifestId, ManifestRef};

    fn reference(id: &str, kind: &str) -> ManifestRef {
        ManifestRef::new(ManifestId::new(id).unwrap(), kind).unwrap()
    }

    fn asset_with_status(status: FactorStatus) -> FactorAsset {
        let now = Utc::now();
        FactorAsset {
            factor_id: "factor-1".to_string(),
            factor_type: FactorType::Formula,
            ast: FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string())),
            lineage: FactorLineage {
                parent_factor_ids: vec![],
                source_engine: "manual".to_string(),
                search_manifest_id: ManifestId::new("search-1").unwrap(),
            },
            data_manifest: reference("data-1", "data_manifest"),
            feature_manifest: reference("feature-1", "feature_manifest"),
            label_manifest: reference("label-1", "label_manifest"),
            evaluation_manifests: vec![reference("eval-1", "evaluation_manifest")],
            metrics: FactorMetrics {
                rank_ic: Some(0.03),
                icir: Some(1.2),
                net_sharpe: Some(1.5),
                max_drawdown: Some(0.05),
                turnover: Some(2.0),
                custom: BTreeMap::new(),
            },
            correlation_cluster: None,
            regime_metrics: BTreeMap::new(),
            symbol_metrics: BTreeMap::new(),
            promotion_status: status,
            live_decay_state: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn rejects_empty_factor_id() {
        let mut asset = asset_with_status(FactorStatus::Generated);
        asset.factor_id = " ".to_string();
        assert_eq!(asset.validate().unwrap_err(), FactorBankError::EmptyFactorId);
    }

    #[test]
    fn blocks_live_full_candidate_execution_in_mvp() {
        let asset = asset_with_status(FactorStatus::LiveFullCandidate);
        assert_eq!(
            asset.validate().unwrap_err(),
            FactorBankError::LiveFullCandidateNotExecutable
        );
    }

    #[test]
    fn rejects_bad_factor_ast() {
        let mut asset = asset_with_status(FactorStatus::Generated);
        asset.ast = FactorAst::Call {
            operator: hft_factor_dsl::FactorOperator::Add,
            args: vec![FactorAst::Terminal(FactorTerminal::Field("oi".to_string()))],
        };

        assert!(matches!(
            asset.validate().unwrap_err(),
            FactorBankError::InvalidFactorAst(_)
        ));
    }
}
