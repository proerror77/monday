import assert from "node:assert/strict";
import { appendFile, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

import type { AgentRunRecord, JsonValue } from "../contracts/operator-contracts.js";
import { harnessContextPath, harnessEventsPath } from "./session-store.js";

export type HarnessLearning = {
  kind: "harness_learning";
  run_id: string;
  cycle_kind: string;
  category:
    | "completion_gap"
    | "tool_gap"
    | "runtime_error"
    | "approval_gate"
    | "negative_result";
  summary: string;
  suggested_change: string;
  subagent_profile?: string;
  created_at: string;
};

const DEFAULT_CONTEXT = `# Harness Meta-Context

This file is maintained by the sidecar from completed agent runs.

## Guardrails

- Live trading, deployment changes, and paper intents stay approval-gated.
- Treat repeated needs_retry as a harness/tool/prompt gap, not as success.
- Candidate subagent/profile changes are proposals until a human lands code.
`;

export async function readHarnessContext(maxChars = 6000): Promise<string> {
  await ensureHarnessEventsLog();
  const path = await harnessContextPath();
  try {
    const body = await readFile(path, "utf8");
    return body.length > maxChars ? body.slice(body.length - maxChars) : body;
  } catch (error: any) {
    if (error?.code !== "ENOENT") throw error;
    await writeFile(path, DEFAULT_CONTEXT, "utf8");
    return DEFAULT_CONTEXT;
  }
}

async function ensureHarnessEventsLog(): Promise<void> {
  await appendFile(await harnessEventsPath(), "", "utf8");
}

export function deriveHarnessLearning(record: AgentRunRecord): HarnessLearning | null {
  const outputSummary = asRecord(record.output_summary);
  const contractEvaluation = asRecord(outputSummary?.contract_evaluation);
  const checks = Array.isArray(contractEvaluation?.checks) ? contractEvaluation.checks : [];
  const needsRetry = checks.map(asRecord).find((check) => check?.status === "needs_retry");
  const blocked = checks.map(asRecord).find((check) => check?.status === "blocked");

  if (record.status === "needs_retry" && needsRetry) {
    return learning(record, classifyCheck(needsRetry), checkDetail(needsRetry), {
      suggested_change: suggestedChange(needsRetry),
      subagent_profile: subagentProfile(needsRetry),
    });
  }
  if (record.status === "blocked" && blocked) {
    return learning(record, "approval_gate", checkDetail(blocked), {
      suggested_change: "Keep this as a human approval or policy decision; do not auto-evolve into mutation tools.",
    });
  }
  if (record.status === "failed" && record.failure_reason) {
    return learning(record, "runtime_error", record.failure_reason, {
      suggested_change: "Add a narrow recovery note or tool-health check only if this error repeats.",
      subagent_profile: "runtime-recovery",
    });
  }
  if (record.status === "partial") {
    const completion = asRecord(outputSummary?.task_completion);
    const detail = typeof completion?.summary === "string" ? completion.summary : "partial completion";
    return learning(record, "negative_result", detail, {
      suggested_change: "Preserve as negative evidence; retry only when the blocker has changed.",
    });
  }
  return null;
}

export async function recordHarnessLearning(record: AgentRunRecord): Promise<HarnessLearning | null> {
  const learning = deriveHarnessLearning(record);
  if (!learning) return null;

  await appendFile(await harnessEventsPath(), `${JSON.stringify(learning)}\n`, "utf8");
  // ponytail: append-only context; compact into sections if this file becomes noisy.
  await appendFile(
    await harnessContextPath(),
    `\n## ${learning.created_at} ${learning.category}\n\n` +
      `- run: ${learning.run_id} (${learning.cycle_kind})\n` +
      `- summary: ${learning.summary}\n` +
      `- suggested_change: ${learning.suggested_change}\n` +
      (learning.subagent_profile ? `- subagent_profile: ${learning.subagent_profile}\n` : ""),
    "utf8"
  );
  return learning;
}

export async function appendHarnessProposal(proposal: {
  category: HarnessLearning["category"];
  summary: string;
  suggested_change: string;
  subagent_profile?: string;
}): Promise<HarnessLearning> {
  const learning: HarnessLearning = {
    kind: "harness_learning",
    run_id: "agent-proposed",
    cycle_kind: "harness_proposal",
    category: proposal.category,
    summary: proposal.summary,
    suggested_change: proposal.suggested_change,
    subagent_profile: proposal.subagent_profile,
    created_at: new Date().toISOString(),
  };
  await appendFile(await harnessEventsPath(), `${JSON.stringify(learning)}\n`, "utf8");
  await appendFile(
    await harnessContextPath(),
    `\n## ${learning.created_at} ${learning.category}\n\n` +
      `- run: ${learning.run_id} (${learning.cycle_kind})\n` +
      `- summary: ${learning.summary}\n` +
      `- suggested_change: ${learning.suggested_change}\n` +
      (learning.subagent_profile ? `- subagent_profile: ${learning.subagent_profile}\n` : ""),
    "utf8"
  );
  return learning;
}

function learning(
  record: AgentRunRecord,
  category: HarnessLearning["category"],
  summary: string,
  patch: Pick<HarnessLearning, "suggested_change"> & Partial<Pick<HarnessLearning, "subagent_profile">>
): HarnessLearning {
  return {
    kind: "harness_learning",
    run_id: record.run_id,
    cycle_kind: record.cycle_kind,
    category,
    summary,
    suggested_change: patch.suggested_change,
    subagent_profile: patch.subagent_profile,
    created_at: new Date().toISOString(),
  };
}

function classifyCheck(check: Record<string, JsonValue>): HarnessLearning["category"] {
  const name = typeof check.name === "string" ? check.name : "";
  if (name === "completion_signal") return "completion_gap";
  if (name === "approval_gate") return "approval_gate";
  return "tool_gap";
}

function checkDetail(check: Record<string, JsonValue>): string {
  return typeof check.detail === "string" ? check.detail : "contract check did not pass";
}

function suggestedChange(check: Record<string, JsonValue>): string {
  const name = typeof check.name === "string" ? check.name : "";
  if (name === "completion_signal") {
    return "Tighten the run prompt or add a completion-sentinel subagent profile for this strategy family.";
  }
  if (name === "grok_decision" || name === "grok_evidence_tools") {
    return "Split Grok evidence collection into a focused profile before final trade/pass synthesis.";
  }
  if (name === "executable_replay" || name === "runtime_parity") {
    return "Route replay/parity evidence to a focused verification profile before strategy synthesis.";
  }
  return "Review missing tool/context surface before increasing retry count.";
}

function subagentProfile(check: Record<string, JsonValue>): string | undefined {
  const name = typeof check.name === "string" ? check.name : "";
  if (name === "completion_signal") return "completion-sentinel";
  if (name === "grok_decision" || name === "grok_evidence_tools") return "grok-evidence";
  if (name === "executable_replay" || name === "runtime_parity") return "replay-parity";
  return undefined;
}

function asRecord(value: JsonValue | undefined | null): Record<string, JsonValue> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value;
}

async function selfTest() {
  const originalRuns = process.env.PLOY_AGENT_RUNS_FILE;
  const originalContext = process.env.PLOY_HARNESS_CONTEXT_FILE;
  const originalEvents = process.env.PLOY_HARNESS_EVENTS_FILE;
  const dir = await mkdtemp(join(tmpdir(), "ploy-harness-memory-"));

  try {
    process.env.PLOY_AGENT_RUNS_FILE = join(dir, "agent-runs.jsonl");
    process.env.PLOY_HARNESS_CONTEXT_FILE = join(dir, "harness-context.md");
    process.env.PLOY_HARNESS_EVENTS_FILE = join(dir, "harness-events.jsonl");
    assert.equal(await readHarnessContext(), DEFAULT_CONTEXT);
    assert.equal(await readFile(await harnessEventsPath(), "utf8"), "");

    const record = {
      run_id: "agent-test",
      cycle_kind: "agentic_strategy",
      status: "needs_retry",
      failure_reason: null,
      output_summary: {
        contract_evaluation: {
          checks: [
            {
              name: "grok_evidence_tools",
              status: "needs_retry",
              detail: "missing WebSearch",
            },
          ],
        },
      },
    } as Partial<AgentRunRecord> as AgentRunRecord;

    const derived = deriveHarnessLearning(record);
    assert.equal(derived?.category, "tool_gap");
    assert.equal(derived?.subagent_profile, "grok-evidence");
    await recordHarnessLearning(record);
    assert.match(await readHarnessContext(), /grok-evidence/);
    assert.match(await readFile(await harnessEventsPath(), "utf8"), /missing WebSearch/);
  } finally {
    restoreEnv("PLOY_AGENT_RUNS_FILE", originalRuns);
    restoreEnv("PLOY_HARNESS_CONTEXT_FILE", originalContext);
    restoreEnv("PLOY_HARNESS_EVENTS_FILE", originalEvents);
    await rm(dir, { recursive: true, force: true });
  }
}

function restoreEnv(key: string, value: string | undefined) {
  if (value === undefined) {
    delete process.env[key];
  } else {
    process.env[key] = value;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await selfTest();
}
