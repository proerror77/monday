const LABEL_DEFINITIONS = {
  "decision:pending": {
    color: "fbca04",
    description: "Research evidence exists, but no promotion/rejection decision has been made",
  },
  "decision:continue": {
    color: "0e8a16",
    description: "Research evidence supports continuing the current line",
  },
  "decision:collect-more": {
    color: "fbca04",
    description: "Research evidence is promising but underpowered",
  },
  "decision:promote": {
    color: "0e8a16",
    description: "Research evidence supports promotion to implementation or runtime",
  },
  "decision:reject": {
    color: "b60205",
    description: "Research evidence rejects the hypothesis",
  },
  "decision:revise": {
    color: "d93f0b",
    description: "Research evidence requires hypothesis or config revision",
  },
  "decision:fix-data": {
    color: "d93f0b",
    description: "Research evidence is blocked by data quality or source issues",
  },
  "decision:fix-runtime": {
    color: "d93f0b",
    description: "Research evidence is blocked by replay/runtime mismatch",
  },
  "decision:fix-workflow": {
    color: "d93f0b",
    description: "Research evidence is blocked by workflow or artifact defects",
  },
  "evidence:backtest": {
    color: "1d76db",
    description: "Issue has backtest workflow evidence",
  },
  "evidence:diagnostic": {
    color: "1d76db",
    description: "Issue has diagnostic research evidence; not deployable by itself",
  },
  "evidence:executable-replay": {
    color: "1d76db",
    description: "Issue has executable-price replay evidence; parity gates still apply",
  },
  "evidence:factor-review": {
    color: "1d76db",
    description: "Issue has factor review workflow evidence",
  },
  "evidence:factor-attribution": {
    color: "1d76db",
    description: "Issue has factor attribution evidence; not deployable by itself",
  },
  "evidence:walk-forward": {
    color: "1d76db",
    description: "Issue has walk-forward workflow evidence",
  },
  "evidence:optimize": {
    color: "1d76db",
    description: "Issue has optimize workflow evidence",
  },
  "evidence:parity": {
    color: "1d76db",
    description: "Issue has replay/dry-run parity workflow evidence",
  },
  "evidence:missing-artifact": {
    color: "d73a4a",
    description: "Workflow evidence artifact is missing",
  },
  "evidence:missing-metrics": {
    color: "d73a4a",
    description: "Workflow evidence is missing headline metrics",
  },
  "parity:blocked": {
    color: "d73a4a",
    description: "Replay/dry-run parity is not decision-grade yet",
  },
  "parity:ready": {
    color: "0e8a16",
    description: "Replay/dry-run parity exposed strict matching evidence",
  },
};

const MANAGED_STATE_PREFIXES = ["decision:", "parity:", "evidence:missing-"];
const MANAGED_STATE_LABELS = new Set(["evidence:diagnostic", "evidence:executable-replay"]);

function normalizeLabel(name) {
  return String(name || "").trim();
}

function labelsForDecision(decision) {
  const value = String(decision || "").toLowerCase();
  if (!value || value.includes("pending")) return ["decision:pending"];
  if (value.includes("promote")) return ["decision:promote"];
  if (value.includes("reject")) return ["decision:reject"];
  if (value.includes("collect")) return ["decision:collect-more"];
  if (value.includes("revise")) return ["decision:revise"];
  if (value.includes("fix-data-or-runtime")) return ["decision:fix-data", "decision:fix-runtime"];
  if (value.includes("fix-data") || value.includes("data-source")) return ["decision:fix-data"];
  if (value.includes("fix-runtime") || value.includes("runtime-mismatch")) return ["decision:fix-runtime"];
  if (value.includes("fix-workflow") || value.includes("artifact")) return ["decision:fix-workflow"];
  if (value.includes("continue")) return ["decision:continue"];
  return ["decision:pending"];
}

async function ensureLabel({ github, context, core, name }) {
  const definition = LABEL_DEFINITIONS[name] || {
    color: "cfd3d7",
    description: "Managed by research CI/CD evidence workflows",
  };
  try {
    await github.rest.issues.createLabel({
      owner: context.repo.owner,
      repo: context.repo.repo,
      name,
      color: definition.color,
      description: definition.description,
    });
    if (core) core.info(`Created label ${name}`);
  } catch (error) {
    if (error.status !== 422) throw error;
  }
}

async function applyResearchIssueLabels({ github, context, core, issue_number, labels }) {
  const nextLabels = Array.from(new Set((labels || []).map(normalizeLabel).filter(Boolean)));
  for (const name of nextLabels) {
    await ensureLabel({ github, context, core, name });
  }

  const current = await github.paginate(github.rest.issues.listLabelsOnIssue, {
    owner: context.repo.owner,
    repo: context.repo.repo,
    issue_number,
    per_page: 100,
  });
  const nextSet = new Set(nextLabels);

  for (const label of current) {
    const name = label.name;
    const managed = MANAGED_STATE_LABELS.has(name)
      || MANAGED_STATE_PREFIXES.some((prefix) => name.startsWith(prefix));
    if (!managed || nextSet.has(name)) continue;
    await github.rest.issues.removeLabel({
      owner: context.repo.owner,
      repo: context.repo.repo,
      issue_number,
      name,
    });
  }

  if (nextLabels.length > 0) {
    await github.rest.issues.addLabels({
      owner: context.repo.owner,
      repo: context.repo.repo,
      issue_number,
      labels: nextLabels,
    });
  }
  if (core) core.info(`Applied research labels: ${nextLabels.join(", ") || "none"}`);
}

module.exports = {
  applyResearchIssueLabels,
  labelsForDecision,
};
