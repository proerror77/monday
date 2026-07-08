//! Deterministic promotion gate contracts for paper, shadow, and live-small.

use hft_factor_bank::{FactorAsset, FactorStatus};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetStage {
    PaperTrading,
    LiveShadow,
    LiveSmall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateFailure {
    MissingEvaluationManifest,
    MissingRankIc,
    MissingNetSharpe,
    MissingMaxDrawdown,
    RankIcBelowFloor,
    NetSharpeBelowFloor,
    MaxDrawdownAboveCeiling,
    ApprovalRequired,
    NotEligibleForTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionGateInput {
    pub target_stage: TargetStage,
    pub min_rank_ic: f64,
    pub min_net_sharpe: f64,
    pub max_drawdown_ceiling: f64,
    pub first_same_class_approval_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionGateDecision {
    pub passed: bool,
    pub failures: Vec<GateFailure>,
}

pub fn evaluate_promotion(
    asset: &FactorAsset,
    input: &PromotionGateInput,
) -> PromotionGateDecision {
    let mut failures = Vec::new();

    if asset.evaluation_manifests.is_empty() {
        failures.push(GateFailure::MissingEvaluationManifest);
    }

    match asset.metrics.rank_ic {
        Some(value) if value >= input.min_rank_ic => {}
        Some(_) => failures.push(GateFailure::RankIcBelowFloor),
        None => failures.push(GateFailure::MissingRankIc),
    }

    match asset.metrics.net_sharpe {
        Some(value) if value >= input.min_net_sharpe => {}
        Some(_) => failures.push(GateFailure::NetSharpeBelowFloor),
        None => failures.push(GateFailure::MissingNetSharpe),
    }

    match asset.metrics.max_drawdown {
        Some(value) if value <= input.max_drawdown_ceiling => {}
        Some(_) => failures.push(GateFailure::MaxDrawdownAboveCeiling),
        None => failures.push(GateFailure::MissingMaxDrawdown),
    }

    let status_ok = match input.target_stage {
        TargetStage::PaperTrading => matches!(
            asset.promotion_status,
            FactorStatus::QuickTestPassed | FactorStatus::FullBacktestPassed
        ),
        TargetStage::LiveShadow => matches!(asset.promotion_status, FactorStatus::PaperTrading),
        TargetStage::LiveSmall => matches!(asset.promotion_status, FactorStatus::LiveShadow),
    };

    if !status_ok {
        failures.push(GateFailure::NotEligibleForTarget);
    }

    if input.target_stage == TargetStage::LiveSmall && !input.first_same_class_approval_present {
        failures.push(GateFailure::ApprovalRequired);
    }

    PromotionGateDecision {
        passed: failures.is_empty(),
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_factor_bank::{FactorLineage, FactorMetrics, FactorType};
    use hft_factor_dsl::{FactorAst, FactorTerminal};
    use hft_research_manifest::{ManifestId, ManifestRef};
    use std::collections::BTreeMap;

    fn reference(id: &str, kind: &str) -> ManifestRef {
        ManifestRef::new(ManifestId::new(id).unwrap(), kind).unwrap()
    }

    fn asset(status: FactorStatus) -> FactorAsset {
        let now = chrono::Utc::now();
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
                rank_ic: Some(0.04),
                icir: Some(1.3),
                net_sharpe: Some(1.6),
                max_drawdown: Some(0.04),
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
    fn live_small_requires_first_same_class_approval() {
        let input = PromotionGateInput {
            target_stage: TargetStage::LiveSmall,
            min_rank_ic: 0.03,
            min_net_sharpe: 1.0,
            max_drawdown_ceiling: 0.05,
            first_same_class_approval_present: false,
        };
        let decision = evaluate_promotion(&asset(FactorStatus::LiveShadow), &input);
        assert!(!decision.passed);
        assert!(decision.failures.contains(&GateFailure::ApprovalRequired));
    }

    #[test]
    fn live_small_passes_when_metrics_status_and_approval_pass() {
        let input = PromotionGateInput {
            target_stage: TargetStage::LiveSmall,
            min_rank_ic: 0.03,
            min_net_sharpe: 1.0,
            max_drawdown_ceiling: 0.05,
            first_same_class_approval_present: true,
        };
        let decision = evaluate_promotion(&asset(FactorStatus::LiveShadow), &input);
        assert!(decision.passed);
        assert!(decision.failures.is_empty());
    }
}
