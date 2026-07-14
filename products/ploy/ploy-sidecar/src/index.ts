/**
 * Ploy Sidecar — Codex CLI operator client
 *
 * Orchestrates NBA comeback research while staying grounded in the
 * trading-platform control plane:
 * 1. ESPN scan → live games with comeback potential
 * 2. Polymarket search → find corresponding markets
 * 3. Control-plane inspection → system status, deployments, trading state
 * 4. X.com sentiment research → WebSearch for injury/momentum
 * 5. Operator recommendation → deployment-aware recommendation or action
 *
 * Architecture:
 *   Sidecar → Codex CLI / xAI Grok → ployd control plane
 */

import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { tradingOutputSchema } from "./schemas/output.js";
import { runAwaitedPollLoop } from "./runtime/poll-loop.js";
import type {
  AgentRunRecord,
  AgentToolCallRecord,
  DeploymentSummary,
  JsonValue,
  SystemStatus,
  TradingStateSnapshot,
} from "./contracts/operator-contracts.js";
import {
  buildRunRecord,
  newRunId,
  recordAgentRun,
  type AgentTaskCompletion,
} from "./runtime/run-recorder.js";
import {
  claimQueuedAgentRunRequests,
  finalizeNeedsRetry,
  queuedAgentRunAttempt,
  type QueuedAgentRunRequest,
} from "./runtime/run-requests.js";
import { readHarnessContext } from "./runtime/harness-memory.js";
import { sidecarAdmissionLimits, validateAgentRunAdmission } from "./runtime/admission.js";
import { evaluateAgentRunContract } from "./runtime/evaluator.js";
import {
  queryGrokBuilderContext,
  queryGrokStrategyCompletion,
  type GrokBuilderContext,
} from "./runtime/grok.js";
import {
  queryCodexFocusedSubagent,
  queryCodexScanOutput,
  queryCodexStrategyCompletion,
} from "./runtime/codex-cli.js";

// ── Config ──────────────────────────────────────────

const CODEX_MODEL = process.env.CODEX_CLI_MODEL?.trim() || null;
const AGENT_ENGINE = process.env.SIDECAR_AGENT_ENGINE || "codex";
const POLL_INTERVAL = parseInt(process.env.SIDECAR_POLL_INTERVAL_SECS || "300", 10) * 1000;
const DRY_RUN = process.env.SIDECAR_DRY_RUN !== "false";
const PLOY_API = process.env.PLOY_API_URL || "http://localhost:8081";

type RuntimeContext = {
  system: {
    status: string;
    uptime_seconds: number;
    error_count_1h: number;
  } | null;
  trading: {
    tracked_deployments: number;
    pending_intents: number;
    active_orders: number;
    open_positions: number;
    gross_exposure: number;
    net_pnl: number;
    sample: Array<{
      deployment_id: string;
      runtime_mode: string;
      pending_intents: number;
      active_orders: number;
      open_positions: number;
      net_pnl: string;
    }>;
  } | null;
  deployments: {
    total: number;
    running: number;
    paused: number;
    stopped: number;
    sample: Array<{
      deployment_id: string;
      desired_state: string;
      observed_state: string;
    }>;
  } | null;
};

type FocusedSubagentProfile = "grok-evidence" | "replay-parity";

type FocusedSubagentResult = {
  profile: FocusedSubagentProfile;
  status: "success" | "partial" | "blocked" | "failed";
  summary: string;
  toolCalls: AgentToolCallRecord[];
};

function focusedSubagentReceipts(
  profile: FocusedSubagentProfile,
  result: Pick<FocusedSubagentResult, "status" | "toolCalls">
): AgentToolCallRecord[] {
  return [
    { name: `subagent__${profile}`, status: result.status },
    ...result.toolCalls,
  ];
}

async function backendFetchJson<T>(path: string): Promise<T | null> {
  try {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
    };
    if (process.env.PLOY_SIDECAR_AUTH_TOKEN) {
      headers["x-ploy-sidecar-token"] = process.env.PLOY_SIDECAR_AUTH_TOKEN;
    }
    if (process.env.PLOY_API_ADMIN_TOKEN) {
      headers["x-ploy-admin-token"] = process.env.PLOY_API_ADMIN_TOKEN;
    }
    if (process.env.PLOY_API_KEY) {
      headers["Authorization"] = `Bearer ${process.env.PLOY_API_KEY}`;
    }

    const resp = await fetch(`${PLOY_API}${path}`, { headers });
    if (!resp.ok) return null;
    return (await resp.json()) as T;
  } catch {
    return null;
  }
}

async function buildRuntimeContext(): Promise<RuntimeContext> {
  const [system, trading, deployments] = await Promise.all([
    backendFetchJson<SystemStatus>("/api/system/status"),
    backendFetchJson<TradingStateSnapshot[]>("/api/trading/state"),
    backendFetchJson<DeploymentSummary[]>("/api/deployments"),
  ]);

  const tradingSnapshots = Array.isArray(trading) ? trading : [];
  const deploymentSnapshots = Array.isArray(deployments) ? deployments : [];

  return {
    system: system
      ? {
          status: system.status,
          uptime_seconds: system.uptime_seconds,
          error_count_1h: system.error_count_1h,
        }
      : null,
    trading: tradingSnapshots.length
      ? {
          tracked_deployments: tradingSnapshots.length,
          pending_intents: tradingSnapshots.reduce(
            (sum, snapshot) => sum + (snapshot.risk?.pending_intents ?? 0),
            0
          ),
          active_orders: tradingSnapshots.reduce(
            (sum, snapshot) => sum + (snapshot.risk?.active_orders ?? 0),
            0
          ),
          open_positions: tradingSnapshots.reduce(
            (sum, snapshot) => sum + (snapshot.risk?.open_positions ?? 0),
            0
          ),
          gross_exposure: tradingSnapshots.reduce(
            (sum, snapshot) => sum + parseFloat(snapshot.risk?.gross_exposure ?? "0"),
            0
          ),
          net_pnl: tradingSnapshots.reduce(
            (sum, snapshot) => sum + parseFloat(snapshot.pnl?.net_pnl ?? "0"),
            0
          ),
          sample: tradingSnapshots.slice(0, 12).map((snapshot) => ({
            deployment_id: snapshot.deployment_id,
            runtime_mode: snapshot.runtime_mode,
            pending_intents: snapshot.risk?.pending_intents ?? 0,
            active_orders: snapshot.risk?.active_orders ?? 0,
            open_positions: snapshot.risk?.open_positions ?? 0,
            net_pnl: snapshot.pnl?.net_pnl ?? "0",
          })),
        }
      : null,
    deployments: deploymentSnapshots.length
      ? {
          total: deploymentSnapshots.length,
          running: deploymentSnapshots.filter((d) => d.desired_state === "running").length,
          paused: deploymentSnapshots.filter((d) => d.desired_state === "paused").length,
          stopped: deploymentSnapshots.filter((d) => d.desired_state === "stopped").length,
          sample: deploymentSnapshots.slice(0, 12).map((d) => ({
            deployment_id: d.deployment_id,
            desired_state: d.desired_state,
            observed_state: d.observed_state,
          })),
        }
      : null,
  };
}

// ── Main Loop ───────────────────────────────────────

function isStructuredOutput(value: unknown): value is {
  research_reports?: Array<unknown>;
  oversight_alerts?: Array<unknown>;
  operator_recommendations?: Array<unknown>;
} {
  return value !== null && typeof value === "object";
}

function selectSubagentProfiles(
  queued: QueuedAgentRunRequest,
  harnessContext: string
): FocusedSubagentProfile[] {
  const profiles = new Set<FocusedSubagentProfile>();
  const contract = queued.request.run_contract;
  const profile = queued.request.strategy_profile;
  if (
    profile.includes("grok_builder") ||
    contract.includes("requires_grok_decision = true") ||
    harnessContext.includes("subagent_profile: grok-evidence")
  ) {
    profiles.add("grok-evidence");
  }
  if (
    contract.includes("requires_executable_replay = true") ||
    contract.includes("requires_runtime_parity = true") ||
    harnessContext.includes("subagent_profile: replay-parity")
  ) {
    profiles.add("replay-parity");
  }
  return [...profiles].slice(0, 2);
}

function subagentPrompt(
  profile: FocusedSubagentProfile,
  queued: QueuedAgentRunRequest,
  runtimeContext: RuntimeContext
): string {
  if (profile === "grok-evidence") {
    return `Focused subagent profile: grok-evidence.

Collect only the sports/X/market evidence needed for this Strategy Builder run.
Return a compact evidence summary and call complete_task with status success, partial, or blocked.

Objective:
${queued.request.objective}

Run contract:
${queued.request.run_contract}

Runtime context:
${JSON.stringify(runtimeContext, null, 2)}`;
  }
  return `Focused subagent profile: replay-parity.

Collect only replay, backtest, config comparison, and oversight parity evidence for this Strategy Builder run.
Return a compact verification summary and call complete_task with status success, partial, or blocked.

Objective:
${queued.request.objective}

Run contract:
${queued.request.run_contract}

Runtime context:
${JSON.stringify(runtimeContext, null, 2)}`;
}

async function runFocusedSubagent(params: {
  profile: FocusedSubagentProfile;
  queued: QueuedAgentRunRequest;
  runtimeContext: RuntimeContext;
  harnessContext: string;
}): Promise<FocusedSubagentResult> {
  console.log(`  Subagent: ${params.profile}`);

  try {
    const result = await queryCodexFocusedSubagent({
      profile: params.profile,
      prompt: subagentPrompt(params.profile, params.queued, params.runtimeContext),
      runtimeContext: params.runtimeContext,
      harnessContext: params.harnessContext,
    });
    const completion = result.value;
    return {
      profile: params.profile,
      status: completion.status,
      summary: completion.summary,
      toolCalls: [
        { name: `codex_cli__${params.profile}`, status: "called" },
        ...result.tool_calls,
      ],
    };
  } catch (error) {
    return {
      profile: params.profile,
      status: "failed",
      summary: error instanceof Error ? error.message : String(error),
      toolCalls: [{ name: `codex_cli__${params.profile}`, status: "failed" }],
    };
  }
}

async function runQueuedStrategyRequest(queued: QueuedAgentRunRequest): Promise<AgentRunRecord> {
  const startedAt = new Date().toISOString();
  const runtimeContext = await buildRuntimeContext();
  const harnessContext = await readHarnessContext();
  const toolCalls: AgentToolCallRecord[] = [];
  const admissionError = validateAgentRunAdmission(queued.request);
  let turnsRemaining = queued.request.max_turns;
  const consumeTurn = () => {
    if (turnsRemaining <= 0) throw new Error("agent run max_turns exhausted");
    turnsRemaining -= 1;
  };
  let sessionId: string | null = null;
  let totalCostUsd: number | null = null;
  let failureReason: string | null = null;
  let completion: AgentTaskCompletion | null = null;
  const subagentResults: FocusedSubagentResult[] = [];
  let grokApiContext: GrokBuilderContext | null = null;

  console.log(`\n[${startedAt}] Starting queued strategy run ${queued.run_id}`);

  try {
    if (admissionError) throw new Error(admissionError);
      if (queued.request.strategy_profile.includes("grok_builder")) {
        consumeTurn();
      try {
        grokApiContext = await queryGrokBuilderContext({
          objective: queued.request.objective,
          runPacket: queued.request.run_packet,
          runContract: queued.request.run_contract,
        });
        toolCalls.push({
          name: "xai__grok_chat_completions",
          status: grokApiContext ? "called" : "not_configured",
        });
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        toolCalls.push({ name: "xai__grok_chat_completions", status: "failed" });
        subagentResults.push({
          profile: "grok-evidence",
          status: "partial",
          summary: `Grok API failed: ${message}`,
          toolCalls: [],
        });
      }
    }

    for (const profile of selectSubagentProfiles(queued, harnessContext)) {
      consumeTurn();
      const result = await runFocusedSubagent({
        profile,
        queued,
        runtimeContext,
        harnessContext,
      });
      subagentResults.push(result);
      toolCalls.push(...focusedSubagentReceipts(profile, result));
    }

    if (AGENT_ENGINE === "grok") {
      consumeTurn();
      const grokRun = await queryGrokStrategyCompletion({
        objective: queued.request.objective,
        runPacket: queued.request.run_packet,
        runContract: queued.request.run_contract,
        runtimeContext,
        harnessContext,
      });
      completion = grokRun.completion;
      sessionId = `xai:${grokRun.model}`;
      toolCalls.push({ name: "xai__grok_chat_completions", status: "called" });
      grokApiContext = {
        provider: "xai",
        model: grokRun.model,
        summary: grokRun.completion.summary,
      };
      console.log(`  Grok engine completed queued run with status: ${completion.status}`);
    } else {
      consumeTurn();
      const codexRun = await queryCodexStrategyCompletion({
        objective: queued.request.objective,
        runPacket: queued.request.run_packet,
        runContract: queued.request.run_contract,
        runtimeContext,
        harnessContext,
        focusedSubagents: subagentResults,
        grokApiContext,
      });
      completion = codexRun.value;
      sessionId = codexRun.session_id;
      toolCalls.push({ name: "codex_cli__exec", status: "called" });
      toolCalls.push(...codexRun.tool_calls);
      console.log(`  Codex CLI completed queued run with status: ${completion.status}`);
    }
  } catch (error) {
    failureReason = error instanceof Error ? error.message : String(error);
    console.error(`  Error in queued strategy run:`, error);
  }

  const record = buildRunRecord({
    runId: queued.run_id,
    cycleKind: "agentic_strategy",
    startedAt,
    finishedAt: new Date().toISOString(),
    sessionId,
    model: AGENT_ENGINE === "grok" && grokApiContext ? `xai:${grokApiContext.model}` : codexModelLabel(),
    runtimeContext,
    toolCalls,
    structuredOutput: null,
    totalCostUsd,
    failureReason,
    completion,
    request: { ...JSON.parse(JSON.stringify(queued.request)), queue_attempt: queuedAgentRunAttempt(queued) } as JsonValue,
    harnessSubagents: JSON.parse(
      JSON.stringify({
        focused_subagents: subagentResults,
        grok_api: grokApiContext,
      })
    ) as JsonValue,
  });
  return record;
}

async function runScanCycle(): Promise<void> {
  const runId = newRunId();
  const timestamp = new Date().toISOString();
  const toolCalls: AgentToolCallRecord[] = [];
  let sessionId: string | null = null;
  let totalCostUsd: number | null = null;
  let failureReason: string | null = null;
  let resultOutput: unknown = null;
  let completion: AgentTaskCompletion | null = null;
  const runtimeContext = await buildRuntimeContext();
  const harnessContext = await readHarnessContext();
  console.log(`\n[${timestamp}] Starting scan cycle (engine=codex, dry_run=${DRY_RUN})`);

  try {
    const codexRun = await queryCodexScanOutput({
      timestamp,
      runtimeContext,
      harnessContext,
      schema: tradingOutputSchema,
    });
    sessionId = codexRun.session_id;
    resultOutput = codexRun.value;
    toolCalls.push(...codexRun.tool_calls);
    toolCalls.push({ name: "codex_cli__exec", status: "called" });

    // Log structured output
    if (resultOutput) {
      const output = resultOutput as {
        scan_summary?: { games_scanned?: number; comeback_candidates?: number };
        opportunities?: Array<{ action: string; trailing_team: string; deficit: number }>;
        operator_actions?: Array<{ kind: string; target: string; status: string }>;
      };

      console.log(`\n  Summary:`);
      console.log(`    Games scanned: ${output.scan_summary?.games_scanned || 0}`);
      console.log(`    Candidates: ${output.scan_summary?.comeback_candidates || 0}`);
      console.log(`    Opportunities: ${output.opportunities?.length || 0}`);
      console.log(`    Operator actions: ${output.operator_actions?.length || 0}`);

      for (const opp of output.opportunities || []) {
        console.log(
          `    → ${opp.trailing_team} (down ${opp.deficit}) — ${opp.action}`
        );
      }

      for (const action of output.operator_actions || []) {
        console.log(
          `    ★ Action: ${action.kind} ${action.target} — ${action.status}`
        );
      }
    }
  } catch (err) {
    failureReason = err instanceof Error ? err.message : String(err);
    console.error(`  Error in scan cycle:`, err);
  } finally {
    await recordAgentRun(
      buildRunRecord({
        runId,
        cycleKind: "research_oversight",
        startedAt: timestamp,
        finishedAt: new Date().toISOString(),
        sessionId,
        model: codexModelLabel(),
        runtimeContext,
        toolCalls,
        structuredOutput: isStructuredOutput(resultOutput) ? resultOutput : null,
        totalCostUsd,
        failureReason,
        completion,
      })
    );
  }
}

async function runSidecarCycle(): Promise<void> {
  const batch = await claimQueuedAgentRunRequests();
  if (batch) {
    for (const request of batch.requests) {
      const record = await runQueuedStrategyRequest(request);
      if (record.status === "needs_retry") {
        const reason = retryReason(record);
        const retry = await finalizeNeedsRetry({
          queued: request,
          reason,
          recordTerminal: () => recordAgentRun(record),
          checkpoint: () => batch.complete(request),
        });
        if (retry) {
          console.warn(
            `  Requeued ${request.run_id} after needs_retry (${queuedAgentRunAttempt(retry)}/${process.env.SIDECAR_AGENT_RUN_MAX_RETRIES || "1"}): ${reason}`
          );
        } else {
          console.warn(`  Retry limit reached for ${request.run_id}: ${reason}`);
        }
      } else {
        await recordAgentRun(record);
        await batch.complete(request);
      }
    }
    await batch.acknowledge();
  }
  await runScanCycle();
}

function retryReason(record: AgentRunRecord): string {
  const outputSummary = asJsonRecord(record.output_summary);
  const contractEvaluation = asJsonRecord(outputSummary?.contract_evaluation);
  const checks = Array.isArray(contractEvaluation?.checks) ? contractEvaluation.checks : [];
  for (const checkValue of checks) {
    const check = asJsonRecord(checkValue);
    if (check?.status === "needs_retry") {
      const name = typeof check.name === "string" ? check.name : "contract";
      const detail = typeof check.detail === "string" ? check.detail : "needs retry";
      return `${name}: ${detail}`;
    }
  }
  return "contract evaluation requested retry";
}

function asJsonRecord(value: JsonValue | undefined): Record<string, JsonValue> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value;
}

function codexModelLabel() {
  return `codex-cli:${CODEX_MODEL || "default"}`;
}

// ── Entry Point ─────────────────────────────────────

async function main() {
  console.log("╔════════════════════════════════════════╗");
  console.log("║  Ploy Sidecar — Operator Client        ║");
  console.log("║  NBA Research + Deployment Console     ║");
  console.log("╚════════════════════════════════════════╝");
  console.log(`  Engine: ${AGENT_ENGINE}`);
  console.log(`  Codex model: ${CODEX_MODEL || "default config"}`);
  console.log(`  Dry run: ${DRY_RUN}`);
  console.log(`  Poll interval: ${POLL_INTERVAL / 1000}s`);
  const limits = sidecarAdmissionLimits();
  console.log(`  Max turns/run: ${limits.maxTurns}`);
  console.log(`  Max budget/cycle: $${limits.maxBudgetUsd}`);
  console.log("");

  await runAwaitedPollLoop(runSidecarCycle, () => delay(POLL_INTERVAL));
}

function selfTest() {
  const receipts = focusedSubagentReceipts("replay-parity", {
    status: "success",
    toolCalls: [
      { name: "codex_cli__replay-parity", status: "called" },
      { name: "mcp__research__run_backtest", status: "completed" },
    ],
  });
  const evaluation = evaluateAgentRunContract({
    request: { run_contract: "requires_executable_replay = true" },
    toolCalls: receipts,
    completion: null,
    failureReason: null,
  });
  assert.equal(evaluation?.status, "passed", "focused_subagent_mcp_receipt_satisfies_run_contract");
}

if (process.env.SIDECAR_SELF_TEST === "true") {
  selfTest();
} else {
  main().catch((err) => {
    console.error("Fatal error:", err);
    process.exit(1);
  });
}
