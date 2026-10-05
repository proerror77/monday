//! Research/governance handoff. Preserve the scientific bundle as audit evidence,
//! then project only fixed executable state for runtime intake.
use crate::{
    CexRuntimeContractV1, DomainError, EvaluationCostsV1, StrategyBundle, StrategyBundleArtifact,
};
use governance::runtime_bundle::{
    RuntimeArtifact, RuntimeBundle, RuntimeCexStrategy, RuntimeCosts, RuntimeExecutionContract,
    RuntimeFrozenStrategy,
};

fn costs(costs: &EvaluationCostsV1) -> RuntimeCosts {
    RuntimeCosts {
        fee_bps: costs.fee_bps,
        rebate_bps: costs.rebate_bps,
        funding_bps: costs.funding_bps,
        latency_bps: costs.latency_bps,
        slippage_bps: costs.slippage_bps,
        cross_spread: costs.cross_spread,
        position_notional_usd: costs.position_notional_usd,
        capacity_depth_levels: costs.capacity_depth_levels,
        max_book_depth_fraction: costs.max_book_depth_fraction,
    }
}
fn contract(contract: CexRuntimeContractV1) -> RuntimeExecutionContract {
    RuntimeExecutionContract {
        zero_epsilon: contract.zero_epsilon,
        observation_frequency_millis: contract.observation_frequency_millis,
        tick_size: contract.tick_size,
        step_size: contract.step_size,
        min_notional: contract.min_notional,
        costs: costs(&contract.costs),
    }
}
impl StrategyBundle {
    /// This never authorizes deployment. The existing persisted promotion,
    /// approvals, signed envelope and runtime hard limits remain separate gates.
    pub fn to_runtime_bundle(&self) -> Result<RuntimeBundle, DomainError> {
        self.validate()?;
        let artifact = match &self.artifact {
            StrategyBundleArtifact::ProbabilityReversal { spec } => {
                RuntimeArtifact::ProbabilityReversal { spec: spec.clone() }
            }
            StrategyBundleArtifact::Formula { ast } => {
                RuntimeArtifact::Formula { ast: ast.clone() }
            }
            StrategyBundleArtifact::Onnx { model } => RuntimeArtifact::Onnx {
                model: model.clone(),
            },
            StrategyBundleArtifact::CexFourStage { strategy } => RuntimeArtifact::CexFourStage {
                strategy: RuntimeCexStrategy {
                    mission_id: strategy.mission_id.clone(),
                    precommit_id: strategy.precommit_id.clone(),
                    venue: strategy.venue.as_str().into(),
                    market: strategy.market.as_str().into(),
                    symbol: strategy.symbol.clone(),
                    executable_formula: strategy.executable_formula.clone(),
                    execution: contract(strategy.runtime_contract_from_validated()?),
                },
            },
            StrategyBundleArtifact::FrozenModel { strategy } => RuntimeArtifact::FrozenModel {
                strategy: Box::new(RuntimeFrozenStrategy {
                    mission_id: strategy.mission_id.clone(),
                    precommit_id: strategy.precommit_id.clone(),
                    program: strategy.frozen.program.clone(),
                    execution: contract(strategy.runtime_contract_from_validated()),
                }),
            },
        };
        RuntimeBundle {
            schema_version: "monday.runtime_bundle.v1".into(),
            source_bundle_hash: self.bundle_hash.clone(),
            bundle_id: self.bundle_id.clone(),
            candidate_id: self.candidate_id.clone(),
            candidate_content_hash: self.candidate_content_hash.clone(),
            dataset_manifest_id: self.dataset_manifest_id.clone(),
            evaluator_version: self.evaluator_version.clone(),
            evaluation_protocol_hash: self.evaluation_protocol_hash.clone(),
            evaluator_config_hash: self.evaluator_config_hash.clone(),
            evaluation_metrics_hash: self.evaluation_metrics_hash.clone(),
            sealed_evaluation_hash: self.sealed_evaluation_hash.clone(),
            artifact,
            bundle_hash: String::new(),
            created_at: self.created_at,
        }
        .finalize()
        .map_err(|_| DomainError::InvalidStrategyBundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> StrategyBundle {
        serde_json::from_str(include_str!(
            "../../../apps/live/tests/fixtures/cex-four-stage-bundle.json"
        ))
        .unwrap()
    }
    #[test]
    fn fixed_runtime_projection_preserves_execution_and_excludes_research_evaluation() {
        let source = source();
        let projected = source.to_runtime_bundle().unwrap();
        let StrategyBundleArtifact::CexFourStage { strategy: original } = &source.artifact else {
            panic!("four-stage source")
        };
        let RuntimeArtifact::CexFourStage { strategy: runtime } = &projected.artifact else {
            panic!("four-stage projection")
        };
        assert_eq!(runtime.executable_formula, original.executable_formula);
        assert_eq!(
            runtime.execution.tick_size,
            original.instrument_rules.tick_size
        );
        assert_eq!(
            runtime.execution.step_size,
            original.instrument_rules.step_size
        );
        assert_eq!(
            runtime.execution.observation_frequency_millis,
            original
                .runtime_contract()
                .unwrap()
                .observation_frequency_millis
        );
        assert_eq!(projected.source_bundle_hash, source.bundle_hash);
        assert_ne!(projected.bundle_hash, source.bundle_hash);
        projected.validate().unwrap();
        let value = serde_json::to_value(&projected).unwrap();
        assert!(value
            .pointer("/artifact/CexFourStage/strategy/strategy_artifact_json")
            .is_none());
        assert!(value
            .pointer("/artifact/CexFourStage/strategy/walk_forward_evidence")
            .is_none());
        assert!(
            serde_json::from_value::<RuntimeBundle>(serde_json::to_value(&source).unwrap())
                .is_err()
        );
    }
    #[test]
    fn recalculating_outer_hash_cannot_project_tampered_scientific_evidence() {
        let mut source = source();
        let StrategyBundleArtifact::CexFourStage { strategy } = &mut source.artifact else {
            panic!("source")
        };
        strategy.strategy_artifact_json.push(' ');
        source.bundle_hash = source.calculated_hash().unwrap();
        assert!(source.to_runtime_bundle().is_err());
    }
}
