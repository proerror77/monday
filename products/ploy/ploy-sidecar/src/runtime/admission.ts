import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const HARD_MAX_TURNS = 30;
const HARD_MAX_BUDGET_USD = 1;

export type SidecarAdmissionLimits = {
  maxTurns: number;
  maxBudgetUsd: number;
};

export function sidecarAdmissionLimits(env: NodeJS.ProcessEnv = process.env): SidecarAdmissionLimits {
  return {
    maxTurns: Math.floor(lowerCap(env.SIDECAR_MAX_TURNS, HARD_MAX_TURNS)),
    maxBudgetUsd: lowerCap(env.SIDECAR_MAX_BUDGET_USD, HARD_MAX_BUDGET_USD),
  };
}

export function validateAgentRunAdmission(
  request: { max_turns: unknown; budget_usd: unknown },
  limits = sidecarAdmissionLimits()
): string | null {
  const maxTurns = request.max_turns;
  const budgetUsd = request.budget_usd;
  if (
    typeof maxTurns !== "number" ||
    !Number.isFinite(maxTurns) ||
    !Number.isInteger(maxTurns) ||
    maxTurns < 1 ||
    maxTurns > limits.maxTurns ||
    typeof budgetUsd !== "number" ||
    !Number.isFinite(budgetUsd) ||
    budgetUsd <= 0 ||
    budgetUsd > limits.maxBudgetUsd
  ) {
    return `agent run exceeds admission caps (max_turns<=${limits.maxTurns}, budget_usd<=${limits.maxBudgetUsd})`;
  }
  return null;
}

function lowerCap(configured: string | undefined, hardCap: number): number {
  if (configured === undefined || configured.trim() === "") return hardCap;
  const parsed = Number(configured);
  return Number.isFinite(parsed) && parsed > 0 ? Math.min(parsed, hardCap) : hardCap;
}

function selfTest() {
  assert.deepEqual(sidecarAdmissionLimits({ SIDECAR_MAX_BUDGET_USD: "5" }), {
    maxTurns: 30,
    maxBudgetUsd: 1,
  }, "configured_caps_cannot_raise_hard_caps");
  assert.equal(validateAgentRunAdmission({ max_turns: 30, budget_usd: 1 }), null);
  for (const invalid of [
    { max_turns: 0, budget_usd: 1 },
    { max_turns: 1.5, budget_usd: 1 },
    { max_turns: Number.NaN, budget_usd: 1 },
    { max_turns: Number.POSITIVE_INFINITY, budget_usd: 1 },
    { max_turns: 1, budget_usd: 0 },
    { max_turns: 1, budget_usd: Number.NaN },
    { max_turns: 1, budget_usd: Number.POSITIVE_INFINITY },
    { max_turns: undefined, budget_usd: 1 },
    { max_turns: 1, budget_usd: undefined },
  ]) {
    assert.match(validateAgentRunAdmission(invalid) ?? "", /admission caps/, "invalid_agent_limits_are_rejected");
  }
  assert.match(
    validateAgentRunAdmission({ max_turns: 3, budget_usd: 0.3 }, sidecarAdmissionLimits({
      SIDECAR_MAX_TURNS: "2",
      SIDECAR_MAX_BUDGET_USD: "0.25",
    })) ?? "",
    /max_turns<=2, budget_usd<=0.25/,
    "configured_caps_may_lower_hard_caps"
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  selfTest();
}
