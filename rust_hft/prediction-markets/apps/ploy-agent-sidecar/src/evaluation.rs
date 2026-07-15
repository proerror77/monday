use crate::AgentTaskCompletion;
use ploy_operator_contracts::{
    agent_run_contract_value, validate_agent_run_contract, validate_agent_run_create_request,
    AgentRunCreateRequest, AgentToolCallRecord,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const HARD_MAX_TURNS: u32 = 30;
const HARD_MAX_BUDGET_USD: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdmissionLimits {
    pub max_turns: u32,
    pub max_budget_usd: f64,
}

impl AdmissionLimits {
    pub fn from_env() -> Self {
        let max_turns = lower_cap_u32("SIDECAR_MAX_TURNS", HARD_MAX_TURNS);
        let max_budget_usd = lower_cap_f64("SIDECAR_MAX_BUDGET_USD", HARD_MAX_BUDGET_USD);
        Self {
            max_turns,
            max_budget_usd,
        }
    }
}

pub fn validate_admission(
    request: &AgentRunCreateRequest,
    limits: AdmissionLimits,
) -> Option<String> {
    if let Err(reason) = validate_agent_run_create_request(request) {
        return Some(reason);
    }
    if request.max_turns == 0
        || request.max_turns > limits.max_turns
        || !request.budget_usd.is_finite()
        || request.budget_usd <= 0.0
        || request.budget_usd > limits.max_budget_usd
    {
        return Some(format!(
            "agent run exceeds admission caps (max_turns<={}, budget_usd<={})",
            limits.max_turns, limits.max_budget_usd
        ));
    }
    let undeployed = [
        "requires_data_audit",
        "requires_grok_decision",
        "requires_executable_replay",
        "requires_full_depth_clob",
        "requires_runtime_parity",
    ]
    .into_iter()
    .filter(|key| contract_enabled(&request.run_contract, key))
    .collect::<Vec<_>>();
    if !undeployed.is_empty() || request.strategy_profile.contains("grok_builder") {
        let mut unavailable = undeployed;
        if request.strategy_profile.contains("grok_builder") {
            unavailable.push("grok_builder_profile");
        }
        return Some(format!(
            "required Rust evidence adapters are not deployed: {}",
            unavailable.join(", ")
        ));
    }
    None
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractCheck {
    pub name: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractEvaluation {
    pub kind: String,
    pub status: String,
    pub checks: Vec<ContractCheck>,
}

pub fn evaluate_agent_run_contract(
    request: Option<&Value>,
    tool_calls: &[AgentToolCallRecord],
    completion: Option<&AgentTaskCompletion>,
    failure_reason: Option<&str>,
) -> Option<ContractEvaluation> {
    let run_contract = request?
        .get("run_contract")?
        .as_str()
        .filter(|value| !value.trim().is_empty())?;
    let mut checks = Vec::new();

    if let Err(reason) = validate_agent_run_contract(run_contract) {
        return Some(ContractEvaluation {
            kind: "agent_run_contract".to_string(),
            status: "blocked".to_string(),
            checks: vec![check("contract_schema", "blocked", &reason)],
        });
    }

    if let Some(reason) = failure_reason {
        checks.push(check("execution_error", "blocked", reason));
    }
    checks.push(completion_check(completion));
    if contract_enabled(run_contract, "requires_data_audit") {
        checks.push(required_tool_check(
            "data_audit",
            tool_calls,
            &[
                &["parent__get_system_status"],
                &["parent__get_trading_state"],
                &["parent__list_deployments"],
                &[
                    "parent__polymarket_market_snapshot",
                    "mcp__polymarket__market_snapshot",
                ],
            ],
        ));
    }
    if contract_enabled(run_contract, "requires_grok_decision") {
        checks.push(grok_decision_check(completion));
        checks.push(required_tool_check(
            "grok_evidence_tools",
            tool_calls,
            &[
                &["mcp__espn__scoreboard", "mcp__espn__game_details"],
                &[
                    "mcp__polymarket__search_markets",
                    "mcp__polymarket__market_snapshot",
                ],
                &["WebSearch", "WebFetch"],
            ],
        ));
    }
    if contract_enabled(run_contract, "requires_executable_replay") {
        checks.push(required_tool_check(
            "executable_replay",
            tool_calls,
            &[&[
                "mcp__research__replay_deployment",
                "mcp__research__run_backtest",
            ]],
        ));
    }
    if contract_enabled(run_contract, "requires_full_depth_clob") {
        checks.push(required_tool_check(
            "full_depth_clob",
            tool_calls,
            &[&[
                "parent__polymarket_full_depth_clob",
                "mcp__polymarket__get_order_book",
                "mcp__polymarket__market_snapshot",
            ]],
        ));
    }
    if contract_enabled(run_contract, "requires_runtime_parity") {
        checks.push(required_tool_check(
            "runtime_parity",
            tool_calls,
            &[
                &["mcp__research__compare_configs"],
                &["mcp__research__check_oversight"],
            ],
        ));
    }
    if contract_enabled(run_contract, "requires_operator_approval") {
        let mut mutating = tool_calls
            .iter()
            .filter(|call| {
                [
                    "submit_paper_intent",
                    "apply_deployment",
                    "set_deployment_state",
                    "place_order",
                    "create_order",
                    "cancel_order",
                    "replace_order",
                    "withdraw",
                    "transfer",
                    "delete_file",
                    "write_file",
                    "command_execution",
                ]
                .iter()
                .any(|blocked| call.name.contains(blocked))
            })
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>();
        let trade_decision =
            completion.and_then(|value| value.decision.as_deref()) == Some("trade");
        checks.push(if mutating.is_empty() && !trade_decision {
            check(
                "approval_gate",
                "passed",
                "no approval-gated mutation tools were called",
            )
        } else if trade_decision {
            check(
                "approval_gate",
                "blocked",
                "trade decision requires explicit human approval",
            )
        } else {
            mutating.sort_unstable();
            check(
                "approval_gate",
                "blocked",
                &format!(
                    "mutating tools called without evaluator approval: {}",
                    mutating.join(", ")
                ),
            )
        });
    }

    let status = if checks.iter().any(|item| item.status == "blocked") {
        "blocked"
    } else if checks.iter().any(|item| item.status == "needs_retry") {
        "needs_retry"
    } else {
        "passed"
    };
    Some(ContractEvaluation {
        kind: "agent_run_contract".to_string(),
        status: status.to_string(),
        checks,
    })
}

pub fn evaluate_structured_output(output: &Value) -> Value {
    let research_reports = array_len(output, "research_reports");
    let oversight_alerts = array_len(output, "oversight_alerts");
    let operator_recommendations = array_len(output, "operator_recommendations");
    let score = research_reports + oversight_alerts + operator_recommendations;
    json!({
        "usefulness": if score >= 3 { "high" } else if score >= 1 { "medium" } else { "low" },
        "research_reports": research_reports,
        "oversight_alerts": oversight_alerts,
        "operator_recommendations": operator_recommendations,
    })
}

pub fn array_len(output: &Value, key: &str) -> usize {
    output
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

fn completion_check(completion: Option<&AgentTaskCompletion>) -> ContractCheck {
    match completion {
        None => check(
            "completion_signal",
            "needs_retry",
            "agent did not return the required completion object",
        ),
        Some(completion) if completion.status == "blocked" => {
            check("completion_signal", "blocked", &completion.summary)
        }
        Some(completion) if completion.status == "partial" => {
            check("completion_signal", "needs_retry", &completion.summary)
        }
        Some(completion) => check("completion_signal", "passed", &completion.summary),
    }
}

fn grok_decision_check(completion: Option<&AgentTaskCompletion>) -> ContractCheck {
    if let Some(decision @ ("trade" | "pass" | "not_queried")) =
        completion.and_then(|item| item.grok_decision.as_deref())
    {
        return check("grok_decision", "passed", &format!("reported {decision}"));
    }
    let summary = completion.map(|item| item.summary.as_str()).unwrap_or("");
    let normalized = summary.to_ascii_lowercase().replace('=', ":");
    let reported = ["trade", "pass", "not_queried"]
        .iter()
        .find(|decision| normalized.contains(&format!("grok_decision: {decision}")));
    match reported {
        Some(decision) => check("grok_decision", "passed", &format!("reported {decision}")),
        None => check(
            "grok_decision",
            "needs_retry",
            "completion must include grok_decision: trade|pass|not_queried",
        ),
    }
}

fn required_tool_check(
    name: &str,
    tool_calls: &[AgentToolCallRecord],
    alternatives: &[&[&str]],
) -> ContractCheck {
    let missing = alternatives
        .iter()
        .filter(|group| {
            !group.iter().any(|tool_name| {
                tool_calls.iter().any(|call| {
                    call.name.contains(tool_name)
                        && matches!(call.status.as_str(), "called" | "success" | "completed")
                })
            })
        })
        .map(|group| group.join(" or "))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        check(name, "passed", "required tools were called")
    } else {
        check(
            name,
            "needs_retry",
            &format!("missing one of: {}", missing.join("; ")),
        )
    }
}

fn check(name: &str, status: &str, detail: &str) -> ContractCheck {
    ContractCheck {
        name: name.to_string(),
        status: status.to_string(),
        detail: detail.to_string(),
    }
}

fn contract_enabled(contract: &str, key: &str) -> bool {
    agent_run_contract_value(contract, key).ok().flatten() == Some("true")
}

fn lower_cap_u32(key: &str, hard_cap: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value.floor().min(hard_cap as f64) as u32)
        .unwrap_or(hard_cap)
}

fn lower_cap_f64(key: &str, hard_cap: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value.min(hard_cap))
        .unwrap_or(hard_cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ploy_operator_contracts::AgentRunCreateRequest;

    fn request() -> AgentRunCreateRequest {
        AgentRunCreateRequest {
            objective: "test".to_string(),
            strategy_profile: "test".to_string(),
            autonomy_mode: "research_until_blocked".to_string(),
            target_evidence: "diagnostic".to_string(),
            symbols: vec![],
            max_turns: 3,
            budget_usd: 0.25,
            run_packet: "packet".to_string(),
            run_contract: "completion_signal = \"required\"".to_string(),
        }
    }

    #[test]
    fn admission_caps_cannot_be_raised() {
        let mut request = request();
        let caps = AdmissionLimits {
            max_turns: 2,
            max_budget_usd: 0.2,
        };
        assert!(validate_admission(&request, caps).is_some());
        request.max_turns = 2;
        request.budget_usd = 0.2;
        assert_eq!(validate_admission(&request, caps), None);
        request.budget_usd = f64::INFINITY;
        assert!(validate_admission(&request, caps).is_some());
    }

    #[test]
    fn evaluator_requires_successful_receipts_and_blocks_mutation() {
        let request = json!({
            "run_contract": "completion_signal = \"required\"\nrequires_executable_replay = true\nrequires_operator_approval = true"
        });
        let completion = AgentTaskCompletion {
            status: "success".to_string(),
            summary: "done".to_string(),
            decision: Some("pass".to_string()),
            grok_decision: None,
            evidence: vec![],
            blockers: vec![],
            next_action: None,
        };
        let needs_retry = evaluate_agent_run_contract(
            Some(&request),
            &[AgentToolCallRecord {
                name: "mcp__research__run_backtest".to_string(),
                status: "failed".to_string(),
            }],
            Some(&completion),
            None,
        )
        .expect("evaluation");
        assert_eq!(needs_retry.status, "needs_retry");

        let blocked = evaluate_agent_run_contract(
            Some(&request),
            &[
                AgentToolCallRecord {
                    name: "mcp__research__run_backtest".to_string(),
                    status: "completed".to_string(),
                },
                AgentToolCallRecord {
                    name: "mcp__ploy__apply_deployment".to_string(),
                    status: "called".to_string(),
                },
            ],
            Some(&completion),
            None,
        )
        .expect("evaluation");
        assert_eq!(blocked.status, "blocked");
    }

    #[test]
    fn unknown_contract_cannot_pass_without_a_completion_sentinel() {
        let completion = AgentTaskCompletion {
            status: "partial".to_string(),
            summary: "not complete".to_string(),
            decision: Some("pass".to_string()),
            grok_decision: None,
            evidence: vec![],
            blockers: vec!["missing contract".to_string()],
            next_action: None,
        };
        let evaluation = evaluate_agent_run_contract(
            Some(&json!({"run_contract": "unknown_contract = true"})),
            &[],
            Some(&completion),
            None,
        )
        .expect("evaluation");
        assert_eq!(evaluation.status, "blocked");
        assert_eq!(evaluation.checks[0].name, "contract_schema");
    }

    #[test]
    fn duplicate_completion_sentinel_is_blocked_consistently() {
        let evaluation = evaluate_agent_run_contract(
            Some(&json!({
                "run_contract": "completion_signal = \"optional\"\ncompletion_signal = \"required\"\nrequires_operator_approval = true"
            })),
            &[],
            None,
            None,
        )
        .expect("evaluation");
        assert_eq!(evaluation.status, "blocked");
        assert!(evaluation.checks[0].detail.contains("duplicate"));
    }

    #[test]
    fn undeployed_evidence_gates_fail_admission_and_are_never_ignored() {
        let mut request = request();
        request.run_contract = "completion_signal = \"required\"\nrequires_data_audit = true\nrequires_full_depth_clob = true".to_string();
        let reason = validate_admission(
            &request,
            AdmissionLimits {
                max_turns: 30,
                max_budget_usd: 1.0,
            },
        )
        .expect("undeployed adapters");
        assert!(reason.contains("requires_data_audit"));
        assert!(reason.contains("requires_full_depth_clob"));

        let evaluation = evaluate_agent_run_contract(
            Some(&json!({"run_contract": request.run_contract})),
            &[],
            None,
            None,
        )
        .expect("evaluation");
        assert_eq!(evaluation.status, "needs_retry");
        assert!(evaluation
            .checks
            .iter()
            .any(|check| check.name == "data_audit"));
        assert!(evaluation
            .checks
            .iter()
            .any(|check| check.name == "full_depth_clob"));
    }
}
