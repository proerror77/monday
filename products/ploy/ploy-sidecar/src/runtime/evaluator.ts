import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

import type { AgentToolCallRecord, JsonValue } from "../contracts/operator-contracts.js";

type StructuredOutput = {
  research_reports?: Array<unknown>;
  oversight_alerts?: Array<unknown>;
  operator_recommendations?: Array<unknown>;
};

export type RunEvaluation = {
  usefulness: "low" | "medium" | "high";
  research_reports: number;
  oversight_alerts: number;
  operator_recommendations: number;
};

export function evaluateRun(output: StructuredOutput): RunEvaluation {
  const researchReports = output.research_reports?.length ?? 0;
  const oversightAlerts = output.oversight_alerts?.length ?? 0;
  const operatorRecommendations = output.operator_recommendations?.length ?? 0;
  const score = researchReports + oversightAlerts + operatorRecommendations;
  return {
    usefulness: score >= 3 ? "high" : score >= 1 ? "medium" : "low",
    research_reports: researchReports,
    oversight_alerts: oversightAlerts,
    operator_recommendations: operatorRecommendations,
  };
}

export type AgentTaskCompletion = {
  status: "success" | "partial" | "blocked";
  summary: string;
  decision?: "continue" | "pass" | "trade" | "monitor" | "blocked";
  grok_decision?: "trade" | "pass" | "not_queried";
  evidence?: string[];
  blockers?: string[];
  next_action?: string;
};

export type ContractCheck = {
  name: string;
  status: "passed" | "needs_retry" | "blocked";
  detail: string;
};

export type ContractEvaluation = {
  kind: "agent_run_contract";
  status: "passed" | "needs_retry" | "blocked";
  checks: ContractCheck[];
};

export function evaluateAgentRunContract(params: {
  request?: JsonValue;
  toolCalls: AgentToolCallRecord[];
  completion: AgentTaskCompletion | null;
  failureReason: string | null;
}): ContractEvaluation | null {
  const request = asRecord(params.request);
  const runContract = typeof request?.run_contract === "string" ? request.run_contract : null;
  if (!runContract) return null;

  const checks: ContractCheck[] = [];
  if (params.failureReason) {
    checks.push({
      name: "execution_error",
      status: "blocked",
      detail: params.failureReason,
    });
  }

  if (contractRequiresCompletion(runContract)) {
    checks.push(completionCheck(params.completion));
  }

  if (contractRequires(runContract, "requires_grok_decision")) {
    checks.push(grokDecisionCheck(params.completion));
    checks.push(requiredToolCheck("grok_evidence_tools", params.toolCalls, [
      ["mcp__espn__scoreboard", "mcp__espn__game_details"],
      ["mcp__polymarket__search_markets", "mcp__polymarket__market_snapshot"],
      ["WebSearch", "WebFetch"],
    ]));
  }

  if (contractRequires(runContract, "requires_executable_replay")) {
    checks.push(requiredToolCheck("executable_replay", params.toolCalls, [
      ["mcp__research__replay_deployment", "mcp__research__run_backtest"],
    ]));
  }

  if (contractRequires(runContract, "requires_runtime_parity")) {
    checks.push(requiredToolCheck("runtime_parity", params.toolCalls, [
      ["mcp__research__compare_configs"],
      ["mcp__research__check_oversight"],
    ]));
  }

  if (contractRequires(runContract, "requires_operator_approval")) {
    const mutatingCalls = params.toolCalls
      .map((call) => call.name)
      .filter((name) =>
        [
          "submit_paper_intent",
          "apply_deployment",
          "set_deployment_state",
        ].some((blocked) => name.includes(blocked))
      );
    checks.push({
      name: "approval_gate",
      status: mutatingCalls.length > 0 ? "blocked" : "passed",
      detail:
        mutatingCalls.length > 0
          ? `mutating tools called without evaluator approval: ${mutatingCalls.join(", ")}`
          : "no approval-gated mutation tools were called",
    });
  }

  return {
    kind: "agent_run_contract",
    status: aggregateStatus(checks),
    checks,
  };
}

function asRecord(value: JsonValue | undefined): Record<string, JsonValue> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value;
}

function contractRequires(contract: string, key: string) {
  return new RegExp(`^\\s*${key}\\s*=\\s*true\\s*$`, "m").test(contract);
}

function contractRequiresCompletion(contract: string) {
  return /^\s*completion_signal\s*=\s*"required"\s*$/m.test(contract);
}

function completionCheck(completion: AgentTaskCompletion | null): ContractCheck {
  if (!completion) {
    return {
      name: "completion_signal",
      status: "needs_retry",
      detail: "agent did not call complete_task",
    };
  }
  if (completion.status === "blocked") {
    return {
      name: "completion_signal",
      status: "blocked",
      detail: completion.summary,
    };
  }
  if (completion.status === "partial") {
    return {
      name: "completion_signal",
      status: "needs_retry",
      detail: completion.summary,
    };
  }
  return {
    name: "completion_signal",
    status: "passed",
    detail: completion.summary,
  };
}

function grokDecisionCheck(completion: AgentTaskCompletion | null): ContractCheck {
  if (
    completion?.grok_decision === "trade" ||
    completion?.grok_decision === "pass" ||
    completion?.grok_decision === "not_queried"
  ) {
    return {
      name: "grok_decision",
      status: "passed",
      detail: `reported ${completion.grok_decision}`,
    };
  }

  const summary = completion?.summary ?? "";
  const match = summary.match(/\bgrok_decision\s*[:=]\s*(trade|pass|not_queried)\b/i);
  return {
    name: "grok_decision",
    status: match ? "passed" : "needs_retry",
    detail: match
      ? `reported ${match[1].toLowerCase()}`
      : "complete_task summary must include grok_decision: trade|pass|not_queried",
  };
}

function requiredToolCheck(
  name: string,
  toolCalls: AgentToolCallRecord[],
  alternatives: string[][]
): ContractCheck {
  const missing = alternatives.filter(
    (group) => !group.some((toolName) => toolCalls.some((call) =>
      call.name.includes(toolName) && ["called", "success", "completed"].includes(call.status)
    ))
  );
  return {
    name,
    status: missing.length === 0 ? "passed" : "needs_retry",
    detail:
      missing.length === 0
        ? "required tools were called"
        : `missing one of: ${missing.map((group) => group.join(" or ")).join("; ")}`,
  };
}

function aggregateStatus(checks: ContractCheck[]): ContractEvaluation["status"] {
  if (checks.some((check) => check.status === "blocked")) return "blocked";
  if (checks.some((check) => check.status === "needs_retry")) return "needs_retry";
  return "passed";
}

function selfTest() {
  const request = {
    run_contract: `
completion_signal = "required"
requires_grok_decision = true
requires_executable_replay = true
requires_runtime_parity = false
requires_operator_approval = true
`,
  };
  const passed = evaluateAgentRunContract({
    request,
    completion: { status: "success", summary: "grok_decision: pass; no trade" },
    failureReason: null,
    toolCalls: [
      { name: "mcp__espn__scoreboard", status: "called" },
      { name: "mcp__polymarket__search_markets", status: "called" },
      { name: "WebSearch", status: "called" },
      { name: "mcp__research__replay_deployment", status: "called" },
    ],
  });
  assert.equal(passed?.status, "passed");

  const needsRetry = evaluateAgentRunContract({
    request,
    completion: { status: "success", summary: "done" },
    failureReason: null,
    toolCalls: [],
  });
  assert.equal(needsRetry?.status, "needs_retry");

  const failedTool = evaluateAgentRunContract({
    request: { run_contract: "requires_executable_replay = true" },
    completion: null,
    failureReason: null,
    toolCalls: [{ name: "mcp__research__replay_deployment", status: "failed" }],
  });
  assert.equal(failedTool?.status, "needs_retry", "matching_failed_tool_does_not_satisfy_contract");

  const blocked = evaluateAgentRunContract({
    request,
    completion: { status: "success", summary: "grok_decision: trade" },
    failureReason: null,
    toolCalls: [{ name: "mcp__ploy-backend__apply_deployment", status: "called" }],
  });
  assert.equal(blocked?.status, "blocked");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  selfTest();
}
