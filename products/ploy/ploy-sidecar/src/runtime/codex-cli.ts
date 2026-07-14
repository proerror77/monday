import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

import type { JsonValue } from "../contracts/operator-contracts.js";
import type { AgentToolCallRecord } from "../contracts/operator-contracts.js";
import type { AgentTaskCompletion } from "./run-recorder.js";

type CommandRunner = (args: string[], prompt: string) => Promise<{
  exitCode: number | null;
  stdout: string;
  stderr: string;
}>;

export type CodexCliResult<T> = {
  provider: "codex-cli";
  model: string;
  session_id: string;
  value: T;
  tool_calls: AgentToolCallRecord[];
};

const COMPLETION_SCHEMA = {
  type: "object",
  additionalProperties: false,
  properties: {
    status: { type: "string", enum: ["success", "partial", "blocked"] },
    summary: { type: "string" },
    decision: { type: "string", enum: ["continue", "pass", "trade", "monitor", "blocked"] },
    grok_decision: { type: "string", enum: ["trade", "pass", "not_queried"] },
    evidence: { type: "array", items: { type: "string" } },
    blockers: { type: "array", items: { type: "string" } },
    next_action: { type: "string" },
  },
  required: [
    "status",
    "summary",
    "decision",
    "grok_decision",
    "evidence",
    "blockers",
    "next_action",
  ],
};

export async function queryCodexStrategyCompletion(params: {
  objective: string;
  runPacket: string;
  runContract: string;
  runtimeContext: unknown;
  harnessContext: string;
  focusedSubagents: unknown;
  grokApiContext: unknown;
  commandRunner?: CommandRunner;
}): Promise<CodexCliResult<AgentTaskCompletion>> {
  const result = await runCodexExec({
    prompt: [
      "You are the Codex CLI execution engine for the Ploy Strategy Builder sidecar.",
      "Return one JSON object only. Do not submit orders, apply deployments, or modify files.",
      "",
      `Objective:\n${params.objective}`,
      `Run packet:\n${params.runPacket}`,
      `Run contract:\n${params.runContract}`,
      `Runtime context:\n${JSON.stringify(params.runtimeContext).slice(0, 6000)}`,
      `Focused subagent findings:\n${JSON.stringify(params.focusedSubagents).slice(0, 4000)}`,
      `Grok API context:\n${JSON.stringify(params.grokApiContext).slice(0, 3000)}`,
      `Harness context:\n${params.harnessContext.slice(0, 4000)}`,
      "",
      'Return JSON with keys: status ("success"|"partial"|"blocked"), summary, decision, grok_decision, evidence array, blockers array, next_action. Use grok_decision "not_queried" when no Grok decision was required.',
    ].join("\n\n"),
    schema: COMPLETION_SCHEMA,
    commandRunner: params.commandRunner,
  });
  return {
    ...result,
    value: parseCompletion(result.value),
  };
}

export async function queryCodexFocusedSubagent(params: {
  profile: string;
  prompt: string;
  runtimeContext: unknown;
  harnessContext: string;
  commandRunner?: CommandRunner;
}): Promise<CodexCliResult<AgentTaskCompletion>> {
  const result = await runCodexExec({
    prompt: [
      `Focused subagent profile: ${params.profile}`,
      "Return one JSON object only. Do not mutate deployments or files.",
      params.prompt,
      `Runtime context:\n${JSON.stringify(params.runtimeContext).slice(0, 6000)}`,
      `Harness context:\n${params.harnessContext.slice(0, 4000)}`,
      'Return JSON with keys: status ("success"|"partial"|"blocked"), summary, decision, grok_decision, evidence array, blockers array, next_action. Use grok_decision "not_queried" when no Grok decision was required.',
    ].join("\n\n"),
    schema: COMPLETION_SCHEMA,
    commandRunner: params.commandRunner,
  });
  return {
    ...result,
    value: parseCompletion(result.value),
  };
}

export async function queryCodexScanOutput(params: {
  timestamp: string;
  runtimeContext: unknown;
  harnessContext: string;
  schema: unknown;
  commandRunner?: CommandRunner;
}): Promise<CodexCliResult<JsonValue>> {
  return runCodexExec({
    prompt: [
      `Current time: ${params.timestamp}`,
      "Run a dry, operator-facing NBA comeback scan from the available runtime context.",
      "Return structured JSON only. Do not submit orders, apply deployments, or modify files.",
      "",
      `Runtime context:\n${JSON.stringify(params.runtimeContext, null, 2).slice(0, 8000)}`,
      `Harness context:\n${params.harnessContext.slice(0, 4000)}`,
    ].join("\n\n"),
    schema: params.schema,
    commandRunner: params.commandRunner,
  });
}

async function runCodexExec(params: {
  prompt: string;
  schema: unknown;
  commandRunner?: CommandRunner;
}): Promise<CodexCliResult<JsonValue>> {
  const dir = await mkdtemp(join(tmpdir(), "ploy-codex-cli-"));
  const schemaPath = join(dir, "schema.json");
  const outputPath = join(dir, "last-message.json");
  try {
    await writeFile(schemaPath, JSON.stringify(params.schema), "utf8");
    const model = process.env.CODEX_CLI_MODEL?.trim();
    const args = [
      "--ask-for-approval",
      "never",
      "exec",
      "--json",
      "--ephemeral",
      "--sandbox",
      process.env.CODEX_CLI_SANDBOX || "read-only",
      "-C",
      process.env.CODEX_CLI_WORKDIR || defaultCodexWorkdir(),
      "--output-schema",
      schemaPath,
      "--output-last-message",
      outputPath,
      ...(model ? ["-m", model] : []),
      "-",
    ];
    const runner = params.commandRunner ?? runCommand;
    const completed = await runner(args, params.prompt);
    if (completed.exitCode !== 0) {
      throw new Error(
        `codex exec exited ${completed.exitCode}: ${
          completed.stderr.trim() || completed.stdout.trim() || "no output"
        }`
      );
    }
    const raw = await readFile(outputPath, "utf8");
    return {
      provider: "codex-cli",
      model: model || "default",
      session_id: "codex-cli",
      value: parseJsonObject(raw),
      tool_calls: parseCodexToolCalls(completed.stdout),
    };
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

export function parseCodexToolCalls(stdout: string): AgentToolCallRecord[] {
  const calls: AgentToolCallRecord[] = [];
  for (const line of stdout.split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const event = JSON.parse(line) as any;
      const item = event.item ?? event.data?.item ?? event;
      const name = item.type === "mcp_tool_call" && typeof item.server === "string" && typeof item.tool === "string"
        ? `mcp__${item.server}__${item.tool}`
        : item.tool_name ?? item.name ?? item.tool;
      const type = String(event.type ?? item.type ?? "");
      if (typeof name === "string" && (type.includes("completed") || type.includes("tool"))) {
        const hasError = nonEmptyError(item.error);
        calls.push({ name, status: type.includes("failed") || item.status === "failed" || hasError ? "failed" : "completed" });
      }
    } catch { /* non-event output */ }
  }
  return calls;
}

function nonEmptyError(error: unknown): boolean {
  if (error === undefined || error === null || error === "") return false;
  if (Array.isArray(error)) return error.length > 0;
  if (typeof error === "object") return Object.keys(error).length > 0;
  return true;
}

function defaultCodexWorkdir() {
  const cwd = process.cwd();
  return basename(cwd) === "ploy-sidecar" ? dirname(cwd) : cwd;
}

function parseCompletion(value: JsonValue): AgentTaskCompletion {
  const candidate = asRecord(value);
  const status = candidate.status;
  return {
    status: status === "partial" || status === "blocked" ? status : "success",
    summary:
      typeof candidate.summary === "string" && candidate.summary.trim()
        ? candidate.summary
        : JSON.stringify(value),
    decision: parseDecision(candidate.decision),
    grok_decision: parseGrokDecision(candidate.grok_decision),
    evidence: parseStringArray(candidate.evidence),
    blockers: parseStringArray(candidate.blockers),
    next_action: typeof candidate.next_action === "string" ? candidate.next_action : undefined,
  };
}

function parseJsonObject(text: string): JsonValue {
  try {
    return JSON.parse(text) as JsonValue;
  } catch {
    const match = text.match(/\{[\s\S]*\}/);
    if (match) return JSON.parse(match[0]) as JsonValue;
    throw new Error("Codex CLI did not return a JSON object");
  }
}

function asRecord(value: JsonValue): Record<string, JsonValue> {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

function parseDecision(value: JsonValue): AgentTaskCompletion["decision"] | undefined {
  return value === "continue" ||
    value === "pass" ||
    value === "trade" ||
    value === "monitor" ||
    value === "blocked"
    ? value
    : undefined;
}

function parseGrokDecision(value: JsonValue): AgentTaskCompletion["grok_decision"] | undefined {
  return value === "trade" || value === "pass" || value === "not_queried" ? value : undefined;
}

function parseStringArray(value: JsonValue): string[] | undefined {
  return Array.isArray(value) && value.every((item) => typeof item === "string")
    ? value
    : undefined;
}

function runCommand(args: string[], prompt: string): Promise<{
  exitCode: number | null;
  stdout: string;
  stderr: string;
}> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.env.CODEX_CLI_BIN || "codex", args, {
      stdio: ["pipe", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    const timeout = setTimeout(() => {
      child.kill("SIGTERM");
      reject(new Error("codex exec timed out"));
    }, Number.parseInt(process.env.CODEX_CLI_TIMEOUT_SECS || "600", 10) * 1000);
    child.stdout.on("data", (chunk) => {
      stdout += String(chunk);
    });
    child.stderr.on("data", (chunk) => {
      stderr += String(chunk);
    });
    child.on("error", (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.on("close", (exitCode) => {
      clearTimeout(timeout);
      resolve({ exitCode, stdout, stderr });
    });
    child.stdin.end(prompt);
  });
}

async function selfTest() {
  const completion = await queryCodexStrategyCompletion({
    objective: "test",
    runPacket: "packet",
    runContract: 'completion_signal = "required"',
    runtimeContext: { ok: true },
    harnessContext: "",
    focusedSubagents: [],
    grokApiContext: null,
    commandRunner: async (args) => {
      assert.deepEqual(args.slice(0, 3), ["--ask-for-approval", "never", "exec"]);
      const outputPath = args[args.indexOf("--output-last-message") + 1];
      await writeFile(
        outputPath,
        '{"status":"success","summary":"done","decision":"pass","evidence":["ok"],"blockers":[]}',
        "utf8"
      );
      return { exitCode: 0, stdout: "", stderr: "" };
    },
  });
  assert.equal(completion.value.status, "success");
  assert.equal(completion.value.summary, "done");
  const calls = parseCodexToolCalls([
    '{"type":"item.completed","item":{"type":"mcp_tool_call","server":"research","tool":"run_backtest","error":null}}',
    '{"type":"item.completed","item":{"type":"mcp_tool_call","server":"research","tool":"compare_configs","error":{"message":"denied"}}}',
  ].join("\n"));
  assert.deepEqual(calls, [
    { name: "mcp__research__run_backtest", status: "completed" },
    { name: "mcp__research__compare_configs", status: "failed" },
  ], "codex_jsonl_mcp_tool_receipts_preserve_success_and_failure");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await selfTest();
}
