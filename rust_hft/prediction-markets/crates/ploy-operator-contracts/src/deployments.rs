use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

fn default_account_id() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentState {
    Enabled,
    Draining,
    Disabled,
    Archived,
}

impl Default for DeploymentState {
    fn default() -> Self {
        Self::Enabled
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    Running,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentRuntimeMode {
    #[default]
    Paper,
    Live,
}

impl std::fmt::Display for DeploymentRuntimeMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Paper => "paper",
            Self::Live => "live",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservedState {
    Starting,
    Running,
    Degraded,
    Paused,
    #[default]
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeploymentSummary {
    pub deployment_id: String,
    #[serde(default)]
    pub runtime_mode: DeploymentRuntimeMode,
    #[serde(default = "default_account_id")]
    pub account_id: String,
    #[serde(default)]
    pub max_gross_exposure: Option<Decimal>,
    #[serde(default)]
    pub deployment_state: DeploymentState,
    pub desired_state: DesiredState,
    pub observed_state: ObservedState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeploymentApplyRequest {
    pub deployment_id: String,
    pub bundle_id: String,
    pub runtime_mode: DeploymentRuntimeMode,
    #[serde(default = "default_account_id")]
    pub account_id: String,
    #[serde(default)]
    pub max_gross_exposure: Option<Decimal>,
    #[serde(default)]
    pub deployment_state: DeploymentState,
    pub desired_state: DesiredState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeploymentControlRequest {
    pub desired_state: Option<DesiredState>,
    pub deployment_state: Option<DeploymentState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeploymentStateSummary {
    pub enabled: usize,
    pub draining: usize,
    pub disabled: usize,
    pub archived: usize,
}

#[cfg(test)]
mod tests {
    use super::{
        DeploymentApplyRequest, DeploymentControlRequest, DeploymentRuntimeMode, DeploymentState,
        DeploymentSummary, DesiredState, ObservedState,
    };
    use rust_decimal::Decimal;
    use serde_json::json;

    #[test]
    fn deployment_state_serializes_as_snake_case() {
        let json = serde_json::to_string(&DeploymentState::Draining).expect("serialize");
        assert_eq!(json, "\"draining\"");
    }

    #[test]
    fn observed_state_serializes_as_running() {
        let json = serde_json::to_string(&ObservedState::Running).expect("serialize");
        assert_eq!(json, "\"running\"");
    }

    #[test]
    fn deployment_summary_uses_stable_wire_keys() {
        let summary = DeploymentSummary {
            deployment_id: "openclaw.default".to_string(),
            runtime_mode: DeploymentRuntimeMode::Paper,
            account_id: "acct-main".to_string(),
            max_gross_exposure: Some(Decimal::new(250, 2)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Degraded,
        };

        let value = serde_json::to_value(summary).expect("to_value");
        assert_eq!(
            value,
            json!({
                "deployment_id": "openclaw.default",
                "runtime_mode": "paper",
                "account_id": "acct-main",
                "max_gross_exposure": "2.50",
                "deployment_state": "enabled",
                "desired_state": "running",
                "observed_state": "degraded",
            })
        );
    }

    #[test]
    fn deployment_apply_request_uses_stable_wire_keys() {
        let value = serde_json::to_value(DeploymentApplyRequest {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: DeploymentRuntimeMode::Paper,
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(Decimal::new(500, 2)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "deployment_id": "example.paper",
                "bundle_id": "example",
                "runtime_mode": "paper",
                "account_id": "acct-paper",
                "max_gross_exposure": "5.00",
                "deployment_state": "enabled",
                "desired_state": "running",
            })
        );
    }

    #[test]
    fn unknown_runtime_mode_is_rejected() {
        let result = serde_json::from_value::<DeploymentApplyRequest>(json!({
            "deployment_id": "example.typo",
            "bundle_id": "example",
            "runtime_mode": "liv",
            "desired_state": "running"
        }));

        assert!(result.is_err());
    }

    #[test]
    fn paper_mode_typo_cannot_launch_live() {
        let result = serde_json::from_value::<DeploymentApplyRequest>(json!({
            "deployment_id": "example.paper",
            "bundle_id": "example",
            "runtime_mode": "papre",
            "desired_state": "running"
        }));

        assert!(result.is_err());
    }

    #[test]
    fn deployment_control_request_serializes_desired_state() {
        let value = serde_json::to_value(DeploymentControlRequest {
            desired_state: Some(DesiredState::Paused),
            deployment_state: Some(DeploymentState::Draining),
        })
        .expect("to_value");
        assert_eq!(
            value,
            json!({ "desired_state": "paused", "deployment_state": "draining" })
        );
    }
}
