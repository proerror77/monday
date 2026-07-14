use crate::deployments::{DeploymentSummary, DesiredState, ObservedState};
use crate::system::SystemStatus;
use crate::trading::TradingStateSnapshot;
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosticsEvidence {
    pub source: String,
    pub label: String,
    pub detail: String,
    pub observed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosticsFinding {
    pub severity: String,
    pub kind: String,
    pub message: String,
    pub first_observed_at: Option<String>,
    pub likely_causes: Vec<String>,
    pub operator_command: Option<String>,
    pub evidence: Vec<DiagnosticsEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlatformDiagnosticsReport {
    pub generated_at: String,
    pub platform_status: String,
    pub first_diverged_metric: Option<String>,
    pub findings: Vec<DiagnosticsFinding>,
    pub recent_evidence: Vec<DiagnosticsEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeploymentDiagnosticsMetrics {
    pub pending_intents: usize,
    pub active_orders: usize,
    pub open_positions: usize,
    pub fills: usize,
    pub positions: usize,
    pub gross_exposure: Decimal,
    pub reserved_order_exposure: Decimal,
    pub total_gross_exposure: Decimal,
    pub net_pnl: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeploymentDiagnosticsReport {
    pub generated_at: String,
    pub deployment_id: String,
    pub bundle_id: String,
    pub runtime_mode: crate::DeploymentRuntimeMode,
    pub account_id: String,
    pub desired_state: String,
    pub observed_state: String,
    pub max_gross_exposure: Option<Decimal>,
    pub primary_diagnosis: String,
    pub first_diverged_metric: Option<String>,
    pub metrics: DeploymentDiagnosticsMetrics,
    pub findings: Vec<DiagnosticsFinding>,
    pub recent_evidence: Vec<DiagnosticsEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalActionKind {
    PauseDeployment,
    DrainDeployment,
    ReduceMaxExposure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Approved,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SafetyProposal {
    pub proposal_id: String,
    pub action_kind: ProposalActionKind,
    pub target_deployment_id: String,
    pub status: ProposalStatus,
    pub rationale: String,
    pub evidence: Vec<String>,
    pub source_run_id: Option<String>,
    pub proposed_max_gross_exposure: Option<Decimal>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
    pub decision_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProposalCreateRequest {
    pub action_kind: ProposalActionKind,
    pub target_deployment_id: String,
    pub rationale: String,
    pub evidence: Vec<String>,
    pub source_run_id: Option<String>,
    pub proposed_max_gross_exposure: Option<Decimal>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProposalDecisionRequest {
    pub decision_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OversightSignal {
    pub severity: String,
    pub kind: String,
    pub message: String,
    pub deployment_id: Option<String>,
    pub evidence: Vec<String>,
    pub recommended_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OversightRecommendedAction {
    pub target: String,
    pub kind: String,
    pub operator_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OversightReport {
    pub timestamp: String,
    pub platform_status: String,
    pub signal_count: usize,
    pub deployments_reviewed: usize,
    pub signals: Vec<OversightSignal>,
    pub recommended_actions: Vec<OversightRecommendedAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OversightSnapshotEvent {
    pub oversight: OversightReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProposalSnapshotEvent {
    pub proposals: Vec<SafetyProposal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentToolCallRecord {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentRunRecord {
    pub run_id: String,
    pub cycle_kind: String,
    pub status: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub session_id: Option<String>,
    pub model: String,
    pub platform_status: Option<String>,
    pub deployment_count: usize,
    pub oversight_signal_count: usize,
    pub oversight_playbook_count: usize,
    pub total_cost_usd: Option<f64>,
    pub tool_calls: Vec<AgentToolCallRecord>,
    pub research_reports: usize,
    pub oversight_alerts: usize,
    pub operator_recommendations: usize,
    pub failure_reason: Option<String>,
    pub runtime_context: Option<serde_json::Value>,
    pub output_summary: Option<serde_json::Value>,
    pub evaluation: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentRunCreateRequest {
    pub objective: String,
    pub strategy_profile: String,
    pub autonomy_mode: String,
    pub target_evidence: String,
    pub symbols: Vec<String>,
    pub max_turns: u32,
    pub budget_usd: f64,
    pub run_packet: String,
    pub run_contract: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AgentRunCreateResponse {
    pub run_id: String,
    pub status: String,
    pub message: String,
}

pub fn compute_oversight_report(
    system: &SystemStatus,
    deployments: &[DeploymentSummary],
    trading: &[TradingStateSnapshot],
) -> OversightReport {
    let mut signals = Vec::new();
    let mut actions = Vec::new();
    let trading_map: BTreeMap<&str, &TradingStateSnapshot> = trading
        .iter()
        .map(|snapshot| (snapshot.deployment_id.as_str(), snapshot))
        .collect();

    for deployment in deployments {
        if deployment.desired_state != DesiredState::Running {
            continue;
        }

        if deployment.observed_state != ObservedState::Running {
            signals.push(OversightSignal {
                severity: "warning".to_string(),
                kind: "state_mismatch".to_string(),
                message: format!(
                    "deployment {} desired {:?} but observed {:?}",
                    deployment.deployment_id, deployment.desired_state, deployment.observed_state
                ),
                deployment_id: Some(deployment.deployment_id.clone()),
                evidence: vec![format!(
                    "desired={:?} observed={:?}",
                    deployment.desired_state, deployment.observed_state
                )],
                recommended_action: "inspect_deployment".to_string(),
            });
            actions.push(OversightRecommendedAction {
                target: deployment.deployment_id.clone(),
                kind: "inspect_deployment".to_string(),
                operator_command: format!(
                    "ployctl deployments inspect {}",
                    deployment.deployment_id
                ),
            });
        }

        if let Some(snapshot) = trading_map.get(deployment.deployment_id.as_str()) {
            if snapshot.risk.active_orders >= 5 {
                signals.push(OversightSignal {
                    severity: "warning".to_string(),
                    kind: "order_buildup".to_string(),
                    message: format!(
                        "deployment {} has {} active orders",
                        deployment.deployment_id, snapshot.risk.active_orders
                    ),
                    deployment_id: Some(deployment.deployment_id.clone()),
                    evidence: vec![format!("active_orders={}", snapshot.risk.active_orders)],
                    recommended_action: "pause_deployment".to_string(),
                });
                actions.push(OversightRecommendedAction {
                    target: deployment.deployment_id.clone(),
                    kind: "pause_deployment".to_string(),
                    operator_command: format!(
                        "ployctl deployments pause {}",
                        deployment.deployment_id
                    ),
                });
            }
        }
    }

    if system.active_alert_count > 0 {
        signals.push(OversightSignal {
            severity: "critical".to_string(),
            kind: "system_alerts".to_string(),
            message: format!("platform has {} active alerts", system.active_alert_count),
            deployment_id: None,
            evidence: vec![format!("active_alert_count={}", system.active_alert_count)],
            recommended_action: "inspect_system".to_string(),
        });
        actions.push(OversightRecommendedAction {
            target: "platform".to_string(),
            kind: "inspect_system".to_string(),
            operator_command: "ployctl system status".to_string(),
        });
    }

    OversightReport {
        timestamp: chrono::Utc::now().to_rfc3339(),
        platform_status: system.status.clone(),
        signal_count: signals.len(),
        deployments_reviewed: deployments.len(),
        signals,
        recommended_actions: actions,
    }
}
