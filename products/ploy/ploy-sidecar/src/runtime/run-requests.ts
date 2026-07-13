import assert from "node:assert/strict";
import { appendFile, mkdir, mkdtemp, readFile, rename, rm, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import type { AgentRunCreateRequest } from "../contracts/operator-contracts.js";
import { agentRunInProgressRequestsPath, agentRunRequestsPath, agentRunsLogPath } from "./session-store.js";

export type QueuedAgentRunRequest = {
  run_id: string;
  created_at: string;
  request: AgentRunCreateRequest;
  attempt?: number;
  last_retry_reason?: string;
  last_retried_at?: string;
};

export type QueuedAgentRunBatch = {
  requests: QueuedAgentRunRequest[];
  acknowledge: () => Promise<void>;
  complete: (request: QueuedAgentRunRequest) => Promise<void>;
};

export function queuedAgentRunAttempt(queued: QueuedAgentRunRequest): number {
  return Number.isInteger(queued.attempt) && (queued.attempt ?? 0) > 0 ? queued.attempt! : 0;
}

export function maxAgentRunRetries(): number {
  const parsed = Number.parseInt(process.env.SIDECAR_AGENT_RUN_MAX_RETRIES || "1", 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 0;
}

export async function requeueAgentRunRequest(
  queued: QueuedAgentRunRequest,
  reason: string
): Promise<QueuedAgentRunRequest | null> {
  const nextAttempt = queuedAgentRunAttempt(queued) + 1;
  if (nextAttempt > maxAgentRunRetries()) return null;

  const retry: QueuedAgentRunRequest = {
    ...queued,
    attempt: nextAttempt,
    last_retry_reason: reason,
    last_retried_at: new Date().toISOString(),
  };
  const path = await agentRunRequestsPath();
  const queuedBody = await readFile(path, "utf8").catch((error: any) => {
    if (error?.code === "ENOENT") return "";
    throw error;
  });
  const retryKey = `${retry.run_id}:${queuedAgentRunAttempt(retry)}`;
  const exists = queuedBody.split(/\r?\n/).some((line) => {
    if (!line.trim()) return false;
    try {
      const candidate = JSON.parse(line) as QueuedAgentRunRequest;
      return `${candidate.run_id}:${queuedAgentRunAttempt(candidate)}` === retryKey;
    } catch {
      return false;
    }
  });
  if (!exists) await appendFile(path, `${JSON.stringify(retry)}\n`, "utf8");
  return retry;
}

export async function finalizeNeedsRetry(params: {
  queued: QueuedAgentRunRequest;
  reason: string;
  recordTerminal: () => Promise<void>;
  checkpoint: () => Promise<void>;
  failpoint?: "after_retry" | "after_terminal";
}): Promise<QueuedAgentRunRequest | null> {
  const retry = await requeueAgentRunRequest(params.queued, params.reason);
  if (params.failpoint === "after_retry") throw new Error("failpoint: after_retry");
  await params.recordTerminal();
  if (params.failpoint === "after_terminal") throw new Error("failpoint: after_terminal");
  await params.checkpoint();
  return retry;
}

export async function claimQueuedAgentRunRequests(): Promise<QueuedAgentRunBatch | null> {
  const path = await agentRunRequestsPath();
  const inProgressPath = await agentRunInProgressRequestsPath();

  const recovered = await readClaimedBatch(inProgressPath);
  if (recovered) return recovered;

  try {
    await rename(path, inProgressPath);
  } catch (error: any) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }

  return readClaimedBatch(inProgressPath);
}

async function readClaimedBatch(path: string): Promise<QueuedAgentRunBatch | null> {
  let body: string;
  try {
    body = await readFile(path, "utf8");
  } catch (error: any) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }

  const terminal = await terminalAttempts();
  const requests: QueuedAgentRunRequest[] = [];
  for (const [index, line] of body.split(/\r?\n/).entries()) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      const request = JSON.parse(trimmed) as QueuedAgentRunRequest;
      if (!terminal.has(`${request.run_id}:${queuedAgentRunAttempt(request)}`)) requests.push(request);
    } catch (error) {
      console.warn(
        `Skipping malformed agent run request on line ${index + 1}: ${
          error instanceof Error ? error.message : String(error)
        }`
      );
    }
  }

  let remaining = requests;
  return {
    requests: remaining,
    complete: async (request) => {
      remaining = remaining.filter((candidate) =>
        !(candidate.run_id === request.run_id && queuedAgentRunAttempt(candidate) === queuedAgentRunAttempt(request))
      );
      if (remaining.length === 0) {
        await unlink(path).catch((error: any) => { if (error?.code !== "ENOENT") throw error; });
        return;
      }
      const temporary = `${path}.tmp`;
      await writeFile(temporary, remaining.map((item) => JSON.stringify(item)).join("\n") + "\n", "utf8");
      await rename(temporary, path);
    },
    acknowledge: () =>
      unlink(path).catch((error: any) => {
        if (error?.code !== "ENOENT") throw error;
      }),
  };
}

async function terminalAttempts(): Promise<Set<string>> {
  const terminal = new Set<string>();
  const body = await readFile(await agentRunsLogPath(), "utf8").catch(() => "");
  for (const line of body.split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const record = JSON.parse(line) as any;
      if (!["requested", "started"].includes(record.status)) {
        terminal.add(`${record.run_id}:${record.runtime_context?.request?.queue_attempt ?? 0}`);
      }
    } catch { /* malformed history */ }
  }
  return terminal;
}

async function selfTest() {
  const originalRequestsPath = process.env.PLOY_AGENT_RUN_REQUESTS_FILE;
  const originalInProgressPath = process.env.PLOY_AGENT_RUN_IN_PROGRESS_FILE;
  const originalRunsPath = process.env.PLOY_AGENT_RUNS_FILE;
  const originalMaxRetries = process.env.SIDECAR_AGENT_RUN_MAX_RETRIES;
  const dir = await mkdtemp(join(tmpdir(), "ploy-run-requests-"));

  try {
    process.env.PLOY_AGENT_RUN_REQUESTS_FILE = join(dir, "queue.jsonl");
    process.env.PLOY_AGENT_RUN_IN_PROGRESS_FILE = join(dir, "in-progress.jsonl");
    process.env.PLOY_AGENT_RUNS_FILE = join(dir, "agent-runs.jsonl");
    process.env.SIDECAR_AGENT_RUN_MAX_RETRIES = "1";

    const request: AgentRunCreateRequest = {
      autonomy_mode: "research_until_blocked",
      budget_usd: 1,
      max_turns: 3,
      objective: "self-test",
      run_contract: 'completion_signal = "required"',
      run_packet: "packet",
      strategy_profile: "self-test",
      symbols: ["TEST"],
      target_evidence: "diagnostic",
    };
    const queued: QueuedAgentRunRequest = {
      run_id: "run-self-test",
      created_at: "2026-07-08T00:00:00.000Z",
      request,
    };

    await appendFile(await agentRunRequestsPath(), `${JSON.stringify(queued)}\n`, "utf8");
    const second = { ...queued, run_id: "run-second" };
    await appendFile(await agentRunRequestsPath(), `${JSON.stringify(second)}\n`, "utf8");
    const claimed = await claimQueuedAgentRunRequests();
    assert.equal(claimed?.requests.length, 2);
    assert.equal(claimed?.requests[0]?.run_id, queued.run_id);

    await appendFile(await agentRunsLogPath(), `${JSON.stringify({
      run_id: queued.run_id,
      status: "needs_retry",
      runtime_context: { request: { queue_attempt: 0 } },
    })}\n`, "utf8");
    const afterCrash = await claimQueuedAgentRunRequests();
    assert.equal(afterCrash?.requests.length, 1, "terminal_attempt_is_not_replayed_after_crash");
    assert.equal(afterCrash?.requests[0]?.run_id, second.run_id);
    await afterCrash?.complete(second);
    assert.equal(await claimQueuedAgentRunRequests(), null);

    const retry = await requeueAgentRunRequest(queued, "completion_signal missing");
    assert.equal(retry?.attempt, 1);
    await requeueAgentRunRequest(queued, "completion_signal missing");
    assert.equal(await requeueAgentRunRequest(retry!, "still missing"), null);
    const retryLines = (await readFile(await agentRunRequestsPath(), "utf8")).trim().split(/\r?\n/);
    assert.equal(retryLines.length, 1, "retry_append_dedupes_run_id_and_attempt");

    await rm(await agentRunRequestsPath(), { force: true });
    await rm(await agentRunInProgressRequestsPath(), { force: true });
    const retryBoundary = { ...queued, run_id: "run-after-retry-boundary" };
    await writeFile(await agentRunInProgressRequestsPath(), `${JSON.stringify(retryBoundary)}\n`, "utf8");
    let terminalWrites = 0;
    let checkpoints = 0;
    const recordTerminal = async () => {
      terminalWrites += 1;
      await appendFile(await agentRunsLogPath(), `${JSON.stringify({
        run_id: retryBoundary.run_id,
        status: "needs_retry",
        runtime_context: { request: { queue_attempt: 0 } },
      })}\n`, "utf8");
    };
    await assert.rejects(finalizeNeedsRetry({
      queued: retryBoundary, reason: "retry", recordTerminal,
      checkpoint: async () => { checkpoints += 1; }, failpoint: "after_retry",
    }), /after_retry/);
    assert.equal(terminalWrites, 0);
    assert.equal(checkpoints, 0);
    assert.equal((await readFile(await agentRunRequestsPath(), "utf8")).trim().split(/\r?\n/).length, 1);
    const recoveredOriginal = await claimQueuedAgentRunRequests();
    assert.equal(recoveredOriginal?.requests[0]?.run_id, retryBoundary.run_id,
      "crash_after_retry_leaves_original_retryable");
    await finalizeNeedsRetry({
      queued: retryBoundary,
      reason: "retry",
      recordTerminal,
      checkpoint: async () => {
        checkpoints += 1;
        await recoveredOriginal?.complete(retryBoundary);
      },
    });
    assert.equal(terminalWrites, 1);
    assert.equal(checkpoints, 1);
    assert.equal((await readFile(await agentRunRequestsPath(), "utf8")).trim().split(/\r?\n/).length, 1,
      "crash_after_retry_never_loses_or_duplicates_retry");

    const boundary = { ...queued, run_id: "run-terminal-boundary" };
    await rm(await agentRunRequestsPath(), { force: true });
    await writeFile(await agentRunInProgressRequestsPath(), `${JSON.stringify(boundary)}\n`, "utf8");
    terminalWrites = 0;
    checkpoints = 0;
    const boundaryRecord = async () => {
      terminalWrites += 1;
      await appendFile(await agentRunsLogPath(), `${JSON.stringify({
        run_id: boundary.run_id,
        status: "needs_retry",
        runtime_context: { request: { queue_attempt: 0 } },
      })}\n`, "utf8");
    };
    await assert.rejects(finalizeNeedsRetry({
      queued: boundary, reason: "retry", recordTerminal: boundaryRecord,
      checkpoint: async () => { checkpoints += 1; },
      failpoint: "after_terminal",
    }), /after_terminal/);
    assert.equal(terminalWrites, 1);
    assert.equal(checkpoints, 0);
    assert.equal((await readFile(await agentRunRequestsPath(), "utf8")).trim().split(/\r?\n/).length, 1,
      "crash_after_terminal_preserves_exactly_one_retry");
    const recoveredAfterTerminal = await claimQueuedAgentRunRequests();
    assert.equal(recoveredAfterTerminal?.requests.length, 0,
      "terminal_history_prevents_original_replay_before_checkpoint");
    await recoveredAfterTerminal?.acknowledge();

    const failingQueue = join(dir, "queue-directory");
    await mkdir(failingQueue);
    process.env.PLOY_AGENT_RUN_REQUESTS_FILE = failingQueue;
    terminalWrites = 0;
    checkpoints = 0;
    await assert.rejects(finalizeNeedsRetry({
      queued: { ...queued, run_id: "run-append-failure" }, reason: "retry",
      recordTerminal: async () => { terminalWrites += 1; },
      checkpoint: async () => { checkpoints += 1; },
    }));
    assert.equal(terminalWrites, 0, "retry_append_failure_does_not_record_terminal");
    assert.equal(checkpoints, 0, "retry_append_failure_does_not_checkpoint");
  } finally {
    if (originalRequestsPath === undefined) {
      delete process.env.PLOY_AGENT_RUN_REQUESTS_FILE;
    } else {
      process.env.PLOY_AGENT_RUN_REQUESTS_FILE = originalRequestsPath;
    }
    if (originalInProgressPath === undefined) {
      delete process.env.PLOY_AGENT_RUN_IN_PROGRESS_FILE;
    } else {
      process.env.PLOY_AGENT_RUN_IN_PROGRESS_FILE = originalInProgressPath;
    }
    if (originalMaxRetries === undefined) {
      delete process.env.SIDECAR_AGENT_RUN_MAX_RETRIES;
    } else {
      process.env.SIDECAR_AGENT_RUN_MAX_RETRIES = originalMaxRetries;
    }
    if (originalRunsPath === undefined) {
      delete process.env.PLOY_AGENT_RUNS_FILE;
    } else {
      process.env.PLOY_AGENT_RUNS_FILE = originalRunsPath;
    }
    await rm(dir, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await selfTest();
}
