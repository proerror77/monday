use ploy_operator_contracts::{
    DeploymentRuntimeMode, DeploymentState, DeploymentSummary, DesiredState, ObservedState,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentRecord {
    pub deployment_id: String,
    #[serde(default)]
    pub bundle_id: String,
    pub runtime_mode: DeploymentRuntimeMode,
    #[serde(default = "default_account_id")]
    pub account_id: String,
    #[serde(default)]
    pub max_gross_exposure: Option<Decimal>,
    #[serde(default)]
    pub deployment_state: DeploymentState,
    pub desired_state: DesiredState,
    #[serde(default)]
    pub observed_state: ObservedState,
}

fn default_account_id() -> String {
    "default".to_string()
}

impl DeploymentRecord {
    pub fn summary(&self) -> DeploymentSummary {
        DeploymentSummary {
            deployment_id: self.deployment_id.clone(),
            runtime_mode: self.runtime_mode.clone(),
            account_id: self.account_id.clone(),
            max_gross_exposure: self.max_gross_exposure,
            deployment_state: self.deployment_state,
            desired_state: self.desired_state,
            observed_state: self.observed_state,
        }
    }
}

#[derive(Debug, Default)]
pub struct DeploymentRegistry {
    deployments: BTreeMap<String, DeploymentRecord>,
}

impl DeploymentRegistry {
    pub fn upsert(&mut self, record: DeploymentRecord) -> &DeploymentRecord {
        let deployment_id = record.deployment_id.clone();
        self.deployments.insert(deployment_id.clone(), record);
        self.deployments
            .get(&deployment_id)
            .expect("deployment inserted")
    }

    pub fn set_desired_state(
        &mut self,
        deployment_id: &str,
        desired_state: DesiredState,
    ) -> Option<&DeploymentRecord> {
        let record = self.deployments.get_mut(deployment_id)?;
        record.desired_state = desired_state;
        Some(record)
    }

    pub fn set_deployment_state(
        &mut self,
        deployment_id: &str,
        deployment_state: DeploymentState,
    ) -> Option<&DeploymentRecord> {
        let record = self.deployments.get_mut(deployment_id)?;
        record.deployment_state = deployment_state;
        Some(record)
    }

    pub fn set_observed_state(
        &mut self,
        deployment_id: &str,
        observed_state: ObservedState,
    ) -> Option<&DeploymentRecord> {
        let record = self.deployments.get_mut(deployment_id)?;
        record.observed_state = observed_state;
        Some(record)
    }

    pub fn set_max_gross_exposure(
        &mut self,
        deployment_id: &str,
        max_gross_exposure: Option<Decimal>,
    ) -> Option<&DeploymentRecord> {
        let record = self.deployments.get_mut(deployment_id)?;
        record.max_gross_exposure = max_gross_exposure;
        Some(record)
    }

    pub fn get(&self, deployment_id: &str) -> Option<&DeploymentRecord> {
        self.deployments.get(deployment_id)
    }

    pub fn remove(&mut self, deployment_id: &str) -> Option<DeploymentRecord> {
        self.deployments.remove(deployment_id)
    }

    pub fn summaries(&self) -> Vec<DeploymentSummary> {
        self.deployments
            .values()
            .map(DeploymentRecord::summary)
            .collect()
    }

    pub fn records(&self) -> Vec<DeploymentRecord> {
        self.deployments.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{DeploymentRecord, DeploymentRegistry};
    use ploy_operator_contracts::{
        DeploymentRuntimeMode, DeploymentState, DesiredState, ObservedState,
    };
    use rust_decimal::Decimal;

    #[test]
    fn create_deployment() {
        let mut registry = DeploymentRegistry::default();
        registry.upsert(DeploymentRecord {
            deployment_id: "openclaw.default".to_string(),
            bundle_id: "openclaw".to_string(),
            runtime_mode: DeploymentRuntimeMode::Paper,
            account_id: "acct-main".to_string(),
            max_gross_exposure: Some(Decimal::new(500, 2)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });

        let record = registry.get("openclaw.default").expect("record");
        assert_eq!(record.bundle_id, "openclaw");
        assert_eq!(record.runtime_mode, DeploymentRuntimeMode::Paper);
        assert_eq!(record.account_id, "acct-main");
        assert_eq!(record.max_gross_exposure, Some(Decimal::new(500, 2)));
    }

    #[test]
    fn update_deployment_states() {
        let mut registry = DeploymentRegistry::default();
        registry.upsert(DeploymentRecord {
            deployment_id: "openclaw.default".to_string(),
            bundle_id: "openclaw".to_string(),
            runtime_mode: DeploymentRuntimeMode::Paper,
            account_id: "acct-main".to_string(),
            max_gross_exposure: Some(Decimal::new(500, 2)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Starting,
        });

        registry.set_desired_state("openclaw.default", DesiredState::Paused);
        registry.set_deployment_state("openclaw.default", DeploymentState::Draining);
        registry.set_observed_state("openclaw.default", ObservedState::Paused);

        let summary = registry.summaries().pop().expect("summary");
        assert_eq!(summary.account_id, "acct-main");
        assert_eq!(summary.max_gross_exposure, Some(Decimal::new(500, 2)));
        assert_eq!(summary.deployment_state, DeploymentState::Draining);
        assert_eq!(summary.desired_state, DesiredState::Paused);
        assert_eq!(summary.observed_state, ObservedState::Paused);
    }
}
