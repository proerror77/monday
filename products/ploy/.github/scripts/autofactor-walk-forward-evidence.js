const fs = require("fs");
const path = require("path");

function readJson(path) {
  if (!fs.existsSync(path)) return null;
  const raw = fs.readFileSync(path, "utf8");
  try {
    return JSON.parse(raw);
  } catch (error) {
    const sanitized = raw
      .replace(/:\s*NaN\b/g, ": null")
      .replace(/:\s*Infinity\b/g, ": null")
      .replace(/:\s*-Infinity\b/g, ": null");
    return JSON.parse(sanitized);
  }
}

function stringifyBlockers(blockers, limit = 3) {
  const values = Array.isArray(blockers) ? blockers.filter(Boolean) : [];
  if (values.length === 0) return "none";
  const shown = values.slice(0, limit);
  const suffix = values.length > shown.length ? `, +${values.length - shown.length} more` : "";
  return `${shown.join("; ")}${suffix}`;
}

function blockerOutcome(blockers) {
  const values = Array.isArray(blockers) ? blockers : [];
  const blockerText = values.join(" ").toLowerCase();

  if (blockerText.includes("data_quality") || blockerText.includes("deribit")) {
    return {
      decision: "fix-data",
      reason: stringifyBlockers(values),
    };
  }
  if (
    blockerText.includes("replay") ||
    blockerText.includes("runtime") ||
    blockerText.includes("mapping") ||
    blockerText.includes("parity")
  ) {
    return {
      decision: "fix-runtime",
      reason: stringifyBlockers(values),
    };
  }
  if (values.length > 0) {
    return {
      decision: "revise",
      reason: stringifyBlockers(values),
    };
  }
  return null;
}

function closedLoopOutcome(closedLoop) {
  if (!closedLoop || typeof closedLoop !== "object") return null;
  const action = String(closedLoop.action || closedLoop.decision || "");
  const decisionByAction = {
    continue_search: "continue-search",
    fix_data: "fix-data",
    fix_runtime: "fix-runtime",
    fix_workflow: "fix-workflow-or-artifact",
    ready_handoff: "promote-to-dry-run",
    revise_prior: "revise",
  };
  const decision = decisionByAction[action];
  if (!decision) return null;
  return {
    decision,
    reason: String(closedLoop.reason || action),
  };
}

function readClosedLoopDecision(artifactDir) {
  const candidates = [
    path.join(artifactDir, "..", "alpha-search-chain", "closed-loop-decision.json"),
    path.join(
      artifactDir,
      "..",
      "factor-walk-forward-v2-upload",
      "alpha-search-chain",
      "closed-loop-decision.json",
    ),
  ];
  for (const candidate of candidates) {
    const payload = readJson(candidate);
    if (payload) return payload;
  }
  return null;
}

function decisionFromArtifacts({ promotion, handoff, closedLoop }) {
  if (!promotion && !handoff) {
    return {
      decision: "fix-workflow-or-artifact",
      reason: "missing AutoFactor promotion and handoff artifacts",
    };
  }

  if (handoff && handoff.status === "ready" && Array.isArray(handoff.strategies) && handoff.strategies.length > 0) {
    return {
      decision: "promote-to-dry-run",
      reason: "qualified AutoFactor strategy handoff is ready",
    };
  }

  const closedLoopDecision = closedLoopOutcome(closedLoop);
  if (closedLoopDecision) return closedLoopDecision;

  const blockerDecision = blockerOutcome(actionableBlockers({ handoff, promotion }));
  if (blockerDecision) return blockerDecision;

  if (promotion && promotion.decision === "blocked") {
    return {
      decision: "revise",
      reason: "no AutoFactor row qualified under the requested target/profile",
    };
  }

  return {
    decision: "pending-review",
    reason: "promotion artifacts were produced but no terminal decision was detected",
  };
}

function topStrategies(handoff, limit = 3) {
  if (!handoff || !Array.isArray(handoff.strategies)) return [];
  return handoff.strategies.slice(0, limit).map((strategy) => {
    const metrics = strategy.metrics || {};
    return [
      strategy.name || "<unknown>",
      strategy.runtime_score || "<missing-runtime-score>",
      `icir=${metrics.icir ?? "n/a"}`,
      `top_bucket_avg_label=${metrics.top_bucket_avg_label ?? "n/a"}`,
    ].join(" | ");
  });
}

function bestEvaluatedFactor(promotion) {
  const evaluated = promotion && Array.isArray(promotion.evaluated_factors)
    ? promotion.evaluated_factors
    : [];
  if (evaluated.length === 0) return null;

  const minimums = promotion.minimums || {};
  const minFill = Number(minimums.top_bucket_full_depth_entry_fill_rate ?? 0.30);
  const candidates = evaluated
    .filter((item) => item && item.factor)
    .map((item, index) => ({ item, index }))
    .sort((left, right) => {
      const leftFactor = left.item.factor || {};
      const rightFactor = right.item.factor || {};
      return Number(Boolean(right.item.qualified)) - Number(Boolean(left.item.qualified))
        || Number(rightFactor.decision === "candidate" && rightFactor.reason === "passed")
          - Number(leftFactor.decision === "candidate" && leftFactor.reason === "passed")
        || Number((rightFactor.top_bucket_full_depth_entry_fill_rate ?? 0) >= minFill)
          - Number((leftFactor.top_bucket_full_depth_entry_fill_rate ?? 0) >= minFill)
        || Number(rightFactor.top_bucket_avg_label ?? -Infinity)
          - Number(leftFactor.top_bucket_avg_label ?? -Infinity)
        || Number(rightFactor.icir ?? -Infinity) - Number(leftFactor.icir ?? -Infinity)
        || left.index - right.index;
    });
  return candidates.length > 0 ? candidates[0].item : null;
}

function actionableBlockers({ handoff, promotion }) {
  const gate = (handoff && handoff.promotion_gate) || (promotion && promotion.promotion_gate) || {};
  const blockedGates = Array.isArray(gate.blocked_gates) ? gate.blocked_gates : [];
  if (handoff && handoff.status === "ready") {
    return blockedGates.filter((item) => {
      const value = String(item || "");
      return !value.startsWith("symbol_holdout:") && !value.startsWith("walk_forward_oos:");
    });
  }
  const gateText = blockedGates.join(" ").toLowerCase();
  if (gateText.includes("data_quality") || gateText.includes("deribit")) {
    return blockedGates;
  }
  const best = bestEvaluatedFactor(promotion);
  if (best && best.factor) {
    const minimums = promotion.minimums || {};
    const minFill = Number(minimums.top_bucket_full_depth_entry_fill_rate ?? 0.30);
    const fillRate = Number(best.factor.top_bucket_full_depth_entry_fill_rate);
    const hasCandidateFillability = Number.isFinite(fillRate) && fillRate >= minFill;
    const candidateBlockers = Array.isArray(best.blockers) ? best.blockers.filter(Boolean) : [];
    if (hasCandidateFillability && candidateBlockers.length > 0) {
      const scopedBlockers = candidateBlockers.filter((item) => {
        const value = String(item || "");
        return value !== "promotion_gate_not_ready"
          && !value.startsWith("global_promotion_gate_not_ready:global_full_depth_entry_fillability:")
          && !value.startsWith("global_full_depth_entry_fillability:");
      });
      if (scopedBlockers.length > 0) return scopedBlockers;
    }
  }
  return blockedGates;
}

function buildWalkForwardEvidence({ title, metadata, artifactDir, runnerLabel }) {
  const promotion = readJson(`${artifactDir}/autofactor-strategy-promotion.json`);
  const handoff = readJson(`${artifactDir}/autofactor-strategy-handoff.json`);
  const closedLoop = readClosedLoopDecision(artifactDir);
  const closedLoopDecision = closedLoopOutcome(closedLoop);
  const outcome = decisionFromArtifacts({ promotion, handoff, closedLoop });
  const gate = (handoff && handoff.promotion_gate) || (promotion && promotion.promotion_gate) || {};
  const strategies = topStrategies(handoff);
  const blockers = closedLoopDecision
    && closedLoopDecision.decision === outcome.decision
    && closedLoop
    && closedLoop.reason
    ? [closedLoop.reason]
    : actionableBlockers({ handoff, promotion });

  const body = [
    `${title}:`,
    "",
    `- Workflow: ${metadata.workflow}`,
    `- Run URL: ${metadata.runUrl}`,
    `- Git ref: ${metadata.gitRef}`,
    `- Source snapshot run: \`${metadata.snapshotRunId}\``,
    `- Dataset/window: ${metadata.startDate} -> ${metadata.endDate}`,
    `- Symbols: ${metadata.symbols}`,
    `- Artifact: \`${metadata.artifactName}\``,
    `- Handoff status: \`${handoff ? handoff.status : "missing"}\``,
    `- Promotion decision: \`${promotion ? promotion.decision : "missing"}\``,
    `- Promotion gate ready: \`${gate.ready === true}\``,
    `- Actionable blockers: \`${stringifyBlockers(blockers)}\``,
    `- Decision: ${outcome.decision}`,
    `- Next action: ${outcome.reason}`,
  ];

  if (strategies.length > 0) {
    body.push("", "Qualified strategies:");
    for (const strategy of strategies) body.push(`- ${strategy}`);
  }

  return {
    body: body.join("\n"),
    decision: outcome.decision,
    labels: ["evidence:walk-forward", runnerLabel].filter(Boolean),
  };
}

module.exports = {
  buildWalkForwardEvidence,
  decisionFromArtifacts,
};
