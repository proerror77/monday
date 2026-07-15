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

pub const AGENT_OBJECTIVE_MAX_BYTES: usize = 4 * 1024;
pub const AGENT_RUN_PACKET_MAX_BYTES: usize = 64 * 1024;
pub const AGENT_RUN_CONTRACT_MAX_BYTES: usize = 32 * 1024;
pub const AGENT_RUN_TOTAL_TEXT_MAX_BYTES: usize = 96 * 1024;
pub const AGENT_SYMBOLS_MAX: usize = 32;

pub fn agent_run_contract_value<'a>(
    contract: &'a str,
    key: &str,
) -> Result<Option<&'a str>, String> {
    let mut value = None;
    for line in contract.lines() {
        let Some((candidate, candidate_value)) = line.trim().split_once('=') else {
            continue;
        };
        if candidate.trim() != key {
            continue;
        }
        if value.is_some() {
            return Err(format!("run_contract contains duplicate `{key}` keys"));
        }
        value = Some(candidate_value.trim());
    }
    Ok(value)
}

pub fn validate_agent_run_contract(contract: &str) -> Result<(), String> {
    if agent_run_contract_value(contract, "completion_signal")? != Some("\"required\"") {
        return Err("run_contract must include completion_signal = \"required\"".to_string());
    }
    for key in [
        "requires_data_audit",
        "requires_grok_decision",
        "requires_executable_replay",
        "requires_full_depth_clob",
        "requires_runtime_parity",
        "requires_operator_approval",
    ] {
        if let Some(value) = agent_run_contract_value(contract, key)? {
            if !matches!(value, "true" | "false") {
                return Err(format!("run_contract `{key}` must be true or false"));
            }
        }
    }
    Ok(())
}

pub fn validate_agent_run_create_request(request: &AgentRunCreateRequest) -> Result<(), String> {
    if request.max_turns == 0
        || request.max_turns > 30
        || !request.budget_usd.is_finite()
        || request.budget_usd <= 0.0
        || request.budget_usd > 1.0
    {
        return Err("max_turns must be 1..=30 and budget_usd must be (0, 1]".to_string());
    }
    if request.objective.trim().is_empty() || request.objective.len() > AGENT_OBJECTIVE_MAX_BYTES {
        return Err(format!(
            "objective must be non-empty and at most {AGENT_OBJECTIVE_MAX_BYTES} bytes"
        ));
    }
    if request.strategy_profile.trim().is_empty() || request.strategy_profile.len() > 256 {
        return Err("strategy_profile must be non-empty and at most 256 bytes".to_string());
    }
    if !matches!(
        request.autonomy_mode.as_str(),
        "research_until_blocked" | "paper_candidate" | "monitor_only"
    ) {
        return Err("autonomy_mode is not supported".to_string());
    }
    if !matches!(
        request.target_evidence.as_str(),
        "diagnostic" | "factor_attribution" | "executable_replay" | "dry_run_candidate"
    ) {
        return Err("target_evidence is not supported".to_string());
    }
    if request.symbols.len() > AGENT_SYMBOLS_MAX
        || request
            .symbols
            .iter()
            .any(|symbol| symbol.trim().is_empty() || symbol.len() > 128)
    {
        return Err(format!(
            "symbols must contain at most {AGENT_SYMBOLS_MAX} non-empty values of at most 128 bytes"
        ));
    }
    if request.run_packet.len() > AGENT_RUN_PACKET_MAX_BYTES {
        return Err(format!(
            "run_packet must be at most {AGENT_RUN_PACKET_MAX_BYTES} bytes"
        ));
    }
    if request.run_contract.len() > AGENT_RUN_CONTRACT_MAX_BYTES {
        return Err(format!(
            "run_contract must be at most {AGENT_RUN_CONTRACT_MAX_BYTES} bytes"
        ));
    }
    validate_agent_run_contract(&request.run_contract)?;
    let total_text_bytes = request
        .objective
        .len()
        .saturating_add(request.strategy_profile.len())
        .saturating_add(request.autonomy_mode.len())
        .saturating_add(request.target_evidence.len())
        .saturating_add(request.run_packet.len())
        .saturating_add(request.run_contract.len())
        .saturating_add(request.symbols.iter().map(String::len).sum::<usize>());
    if total_text_bytes > AGENT_RUN_TOTAL_TEXT_MAX_BYTES {
        return Err(format!(
            "agent request text must be at most {AGENT_RUN_TOTAL_TEXT_MAX_BYTES} bytes"
        ));
    }
    Ok(())
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

#[cfg(test)]
mod agent_run_request_tests {
    use super::*;

    fn request() -> AgentRunCreateRequest {
        AgentRunCreateRequest {
            objective: "bounded research".to_string(),
            strategy_profile: "test.agent".to_string(),
            autonomy_mode: "monitor_only".to_string(),
            target_evidence: "diagnostic".to_string(),
            symbols: vec!["BTC".to_string()],
            max_turns: 3,
            budget_usd: 0.25,
            run_packet: "packet".to_string(),
            run_contract: "completion_signal = \"required\"".to_string(),
        }
    }

    #[test]
    fn agent_run_admission_bounds_text_and_enum_inputs() {
        assert!(validate_agent_run_create_request(&request()).is_ok());

        let mut oversized = request();
        oversized.run_packet = "x".repeat(AGENT_RUN_PACKET_MAX_BYTES + 1);
        assert!(validate_agent_run_create_request(&oversized).is_err());

        let mut invalid = request();
        invalid.autonomy_mode = "live_until_filled".to_string();
        assert!(validate_agent_run_create_request(&invalid).is_err());

        let mut too_many_symbols = request();
        too_many_symbols.symbols = vec!["BTC".to_string(); AGENT_SYMBOLS_MAX + 1];
        assert!(validate_agent_run_create_request(&too_many_symbols).is_err());

        let mut missing_completion_sentinel = request();
        missing_completion_sentinel.run_contract = "requires_operator_approval = true".to_string();
        assert_eq!(
            validate_agent_run_create_request(&missing_completion_sentinel),
            Err("run_contract must include completion_signal = \"required\"".to_string())
        );

        let mut duplicate_completion_sentinel = request();
        duplicate_completion_sentinel.run_contract =
            "completion_signal = \"optional\"\ncompletion_signal = \"required\"".to_string();
        assert_eq!(
            validate_agent_run_create_request(&duplicate_completion_sentinel),
            Err("run_contract contains duplicate `completion_signal` keys".to_string())
        );
    }
}
