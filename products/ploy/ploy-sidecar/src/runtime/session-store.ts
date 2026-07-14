import { mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const DEFAULT_RUN_DIR = "run/sidecar";

export async function agentRunsLogPath(): Promise<string> {
  const logPath = resolve(process.env.PLOY_AGENT_RUNS_FILE || `${DEFAULT_RUN_DIR}/agent-runs.jsonl`);
  await mkdir(dirname(logPath), { recursive: true });
  return logPath;
}

export async function agentRunRequestsPath(): Promise<string> {
  const configured = process.env.PLOY_AGENT_RUN_REQUESTS_FILE;
  const requestPath = configured
    ? resolve(configured)
    : resolve(dirname(await agentRunsLogPath()), "agent-run-requests.jsonl");
  await mkdir(dirname(requestPath), { recursive: true });
  return requestPath;
}

export async function agentRunInProgressRequestsPath(): Promise<string> {
  const requestPath = await agentRunRequestsPath();
  const inProgressPath = process.env.PLOY_AGENT_RUN_IN_PROGRESS_FILE
    ? resolve(process.env.PLOY_AGENT_RUN_IN_PROGRESS_FILE)
    : resolve(dirname(requestPath), "agent-run-requests.in-progress.jsonl");
  await mkdir(dirname(inProgressPath), { recursive: true });
  return inProgressPath;
}

export async function harnessContextPath(): Promise<string> {
  const configured = process.env.PLOY_HARNESS_CONTEXT_FILE;
  const contextPath = configured
    ? resolve(configured)
    : resolve(dirname(await agentRunsLogPath()), "harness-context.md");
  await mkdir(dirname(contextPath), { recursive: true });
  return contextPath;
}

export async function harnessEventsPath(): Promise<string> {
  const configured = process.env.PLOY_HARNESS_EVENTS_FILE;
  const eventsPath = configured
    ? resolve(configured)
    : resolve(dirname(await agentRunsLogPath()), "harness-events.jsonl");
  await mkdir(dirname(eventsPath), { recursive: true });
  return eventsPath;
}
