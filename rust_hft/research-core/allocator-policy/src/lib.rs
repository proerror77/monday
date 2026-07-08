//! Allocator and risk policy proposal contracts.
//!
//! Agents and models may propose weights here. They do not mutate live runtime state.

use hft_live_small_supervisor::{LiveSmallPolicyLimits, LiveSmallSupervisorError};
use hft_research_manifest::{ManifestError, ManifestRef};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AllocatorPolicyError {
    #[error("allocator policy id cannot be empty")]
    EmptyPolicyId,
    #[error("factor id cannot be empty")]
    EmptyFactorId,
    #[error("allocation weight must be finite and non-negative")]
    InvalidWeight,
    #[error("symbol exposure must be finite and non-negative")]
    InvalidSymbolExposure,
    #[error("gross weight is above policy cap")]
    GrossWeightAboveCap,
    #[error("factor weight is above policy cap")]
    FactorWeightAboveCap,
    #[error("symbol exposure is above policy cap")]
    SymbolExposureAboveCap,
    #[error("invalid manifest reference: {0}")]
    InvalidManifest(#[from] ManifestError),
    #[error("invalid live-small policy limits: {0}")]
    InvalidLimits(#[from] LiveSmallSupervisorError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactorAllocation {
    pub factor_id: String,
    pub weight: f64,
}

impl FactorAllocation {
    pub fn validate(&self) -> Result<(), AllocatorPolicyError> {
        if self.factor_id.trim().is_empty() {
            return Err(AllocatorPolicyError::EmptyFactorId);
        }
        if !self.weight.is_finite() || self.weight < 0.0 {
            return Err(AllocatorPolicyError::InvalidWeight);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AllocatorPolicyProposal {
    pub policy_id: String,
    pub source_manifest: ManifestRef,
    pub allocations: Vec<FactorAllocation>,
    pub requested_symbol_exposure: f64,
    pub live_small_limits: LiveSmallPolicyLimits,
}

impl AllocatorPolicyProposal {
    pub fn validate(&self) -> Result<(), AllocatorPolicyError> {
        if self.policy_id.trim().is_empty() {
            return Err(AllocatorPolicyError::EmptyPolicyId);
        }
        self.source_manifest.validate()?;
        self.live_small_limits.validate()?;
        if !self.requested_symbol_exposure.is_finite() || self.requested_symbol_exposure < 0.0 {
            return Err(AllocatorPolicyError::InvalidSymbolExposure);
        }
        if self.requested_symbol_exposure > self.live_small_limits.max_symbol_exposure {
            return Err(AllocatorPolicyError::SymbolExposureAboveCap);
        }

        let mut gross_weight = 0.0;
        for allocation in &self.allocations {
            allocation.validate()?;
            if allocation.weight > self.live_small_limits.max_factor_weight {
                return Err(AllocatorPolicyError::FactorWeightAboveCap);
            }
            gross_weight += allocation.weight;
        }
        if gross_weight > 1.0 {
            return Err(AllocatorPolicyError::GrossWeightAboveCap);
        }
        Ok(())
    }

    pub fn requested_factor_weight(&self) -> f64 {
        self.allocations
            .iter()
            .map(|allocation| allocation.weight)
            .fold(0.0, f64::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_research_manifest::{ManifestId, ManifestRef};

    fn proposal() -> AllocatorPolicyProposal {
        AllocatorPolicyProposal {
            policy_id: "alloc-policy-1".to_string(),
            source_manifest: ManifestRef::new(
                ManifestId::new("promotion-1").unwrap(),
                "promotion_manifest",
            )
            .unwrap(),
            allocations: vec![FactorAllocation {
                factor_id: "factor-1".to_string(),
                weight: 0.05,
            }],
            requested_symbol_exposure: 0.1,
            live_small_limits: LiveSmallPolicyLimits {
                max_factor_weight: 0.1,
                max_symbol_exposure: 0.2,
                max_account_drawdown: 0.03,
            },
        }
    }

    #[test]
    fn accepts_policy_inside_limits() {
        assert_eq!(proposal().validate(), Ok(()));
        assert_eq!(proposal().requested_factor_weight(), 0.05);
    }

    #[test]
    fn rejects_factor_weight_above_cap() {
        let mut proposal = proposal();
        proposal.allocations[0].weight = 0.2;

        assert_eq!(
            proposal.validate().unwrap_err(),
            AllocatorPolicyError::FactorWeightAboveCap
        );
    }

    #[test]
    fn rejects_symbol_exposure_above_cap() {
        let mut proposal = proposal();
        proposal.requested_symbol_exposure = 0.3;

        assert_eq!(
            proposal.validate().unwrap_err(),
            AllocatorPolicyError::SymbolExposureAboveCap
        );
    }
}
