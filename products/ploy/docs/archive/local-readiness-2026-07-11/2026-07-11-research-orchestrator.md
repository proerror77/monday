# Polymarket Research Orchestrator Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans task-by-task. REQUIRED DOMAIN SKILL: use event-ml-automl-workflow when changing research action order or gates.

Goal: Add a Polymarket PM5D/PM15D research-only Agent profile that can inspect evidence and request bounded existing research workflows while being structurally unable to submit/cancel/redeem orders, mutate deployments, or bypass promotion gates.

Architecture: Reuse the Sidecar admission limits, single-flight poll loop, Codex completion adapter, run recorder, and evaluator; move production requests to a PostgreSQL lease queue while retaining file mode for local tests. Model output may request one typed research action; a deterministic adapter maps that action to hard-coded GitHub research workflows or research issues. The model never supplies a workflow name or shell command.

Tech Stack: TypeScript, Node.js `child_process.execFile`, lockfile-pinned official Codex CLI, PostgreSQL/`pg`, local-only Sidecar JSONL adapter, existing GitHub Actions research workflows, and the Strategy Builder frontend.

## Global Constraints

- Start after the horizon-safe research plan lands so PM5D/PM15D profiles share the canonical horizon contract.
- Do not add a generic MCP server, generic workflow engine, autonomous code writer, or third trading runtime.
- Do not provide wallet, order, cancellation, replacement, redeem, deployment control, deploy workflow, arbitrary shell, unrestricted filesystem, or self-modification capability to the research profile.
- Before pinning the production Codex CLI package in Task 3, use the `openai-docs` skill/current official Codex installation source, then commit one exact `@openai/codex` version and lockfile integrity; never use `latest` at deploy time.
- Sidecar queue/run JSONL is transport and diagnostics only. Research OS records, workflow URLs, immutable artifacts, and typed handoffs remain promotion truth.
- Default all external mutation gates to disabled. Local tests use fake command runners and do not create issues, PRs, workflow runs, or deployments.
- A dry-run config PR may only be created by the dedicated deterministic workflow after its handoff reports ready; the Sidecar may dispatch that exact workflow but never writes config, chooses a path, merges, or deploys.
- Keep the NBA/Grok scan profile separate.
- PM1H is not exposed as an Agent strategy profile.
- Each task is one atomic commit and stages only its owned paths.

---

### Task 1: Make the completion contract explicitly research-only

Files:

- Modify `ploy-sidecar/src/runtime/run-recorder.ts`.
- Modify `ploy-sidecar/src/runtime/evaluator.ts`.
- Modify `ploy-sidecar/src/runtime/codex-cli.ts`.
- Modify `ploy-sidecar/src/runtime/grok.ts`.
- Add `ploy-sidecar/src/runtime/research-evidence.ts`.
- Modify `ploy-sidecar/package.json` only if the test command needs to include a new self-test file in a later task.

Typed action request:

```ts
export type ResearchEvidenceStage =
  | "diagnostic"
  | "factor_attribution"
  | "executable_replay"
  | "walk_forward"
  | "runtime_parity"
  | "dry_run_candidate";

export type ResearchHorizonRequest = {
  market_window_secs: 300 | 900;
  prediction_horizon_secs: number;
  entry_offset_secs: number;
  target_label: "settlement_up";
  accounting_lane: "settlement_probability";
  settlement_source: "official_polymarket";
  allowed_symbols: string[];
};

type HorizonScopedResearchAction = {
  horizon: ResearchHorizonRequest;
} & (
  | {
      kind: "trace_plan";
      evidence_stage: ResearchEvidenceStage;
      limit: number;
      issue_number?: number;
    }
  | {
      kind: "execute_plan";
      plan_run_id: string;
      plan_artifact_name?: string;
      execution: "dry_run";
      snapshot_run_id?: string;
      symbols: string[];
      stake_usd: number;
      chain_remaining: number;
      issue_number?: number;
    }
  | {
      kind: "export_event_root_input";
      start_date: string;
      end_date: string;
      symbols: string[];
    }
  | {
      kind: "produce_event_root";
      source_input_run_id: string;
      source_input_artifact_name?: string;
    }
  | {
      kind: "run_event_ml";
      source_dataset_run_id: string;
      source_dataset_artifact_name?: string;
      start_date: string;
      end_date: string;
      symbols: string[];
      child_window_events: 150;
    }
  | {
      kind: "capture_runtime_recording";
      start_date: string;
      end_date: string;
      symbols: string[];
    }
  | {
      kind: "run_candidate_replay";
      source_run_id: string;
      source_artifact_name: string;
      candidate_id: string;
      recording_run_id: string;
      recording_artifact_name: string;
    }
  | {
      kind: "capture_dry_run_evidence";
      candidate_replay_run_id: string;
      candidate_replay_artifact_name: string;
    }
  | {
      kind: "run_recorded_parity";
      candidate_replay_run_id: string;
      candidate_replay_artifact_name: string;
      dry_run_evidence_run_id: string;
      dry_run_evidence_artifact_name: string;
    }
  | {
      kind: "prepare_dry_run_config_pr";
      handoff_run_id: string;
      handoff_artifact_name: string;
      candidate_replay_run_id: string;
      candidate_replay_artifact_name: string;
    }
  | {
      kind: "record_research_decision";
      decision: "continue" | "revise" | "reject";
      rationale: string;
      evidence_ids: string[];
    }
  | {
      kind: "record_typed_prior";
      evidence_ids: string[];
      prior: ResearchTypedPriorDraft;
    }
  | { kind: "create_research_issue"; title: string; body: string }
  | { kind: "comment_research_issue"; issue_number: number; body: string }
);

export type ResearchTypedPriorDraft = {
  schema_version: "research_manager_typed_prior.v1";
  mutations: Array<{
    base_factor: string;
    mutation_type: string;
    name?: string;
    feature?: string;
    denominator_feature?: string;
    constant?: number;
    lo?: number;
    hi?: number;
    window?: number;
  }>;
  runtime_avoid_factors: Array<{
    base_factor: string;
    factor_family?: string;
    runtime_score?: string;
    reason: string;
  }>;
};

export type ResearchActionRequest =
  | { kind: "none"; reason: string }
  | HorizonScopedResearchAction;

export type ResearchEvidenceReference = {
  evidence_stage: ResearchEvidenceStage;
  workflow_url: string;
  artifact_name: string;
  head_sha: string;
  artifact_sha256: string;
  horizon: ResearchHorizonRequest;
};

export type ResearchEvidencePayload =
  | {
      kind: "portable_input";
      manifest_sha256: string;
      source_start: string;
      source_end: string;
      event_count: number;
      observation_count: number;
      chronology_count: number;
    }
  | {
      kind: "event_root_dataset";
      manifest_sha256: string;
      source_portable_manifest_sha256: string;
      event_count: number;
      child_window_count: number;
      child_manifest_sha256: string[];
    }
  | {
      kind: "coverage";
      event_count: number;
      row_count: number;
      distinct_window_count: number;
      missing_feature_rates: Record<string, number>;
      blockers: string[];
    }
  | {
      kind: "research_trace";
      run_id: string;
      evidence_stage: ResearchEvidenceStage;
      decision: "continue" | "revise" | "reject" | "ready" | "blocked";
      blockers: string[];
      artifact_ids: string[];
    }
  | {
      kind: "factor_registry";
      registry_sha256: string;
      accepted: Array<{ name: string; dsl_hash: string; status: string }>;
      rejected: Array<{ name: string; reason: string }>;
      blockers: string[];
    }
  | {
      kind: "candidate";
      candidate_id: string;
      trade_count: number;
      pnl: string;
      roi: string;
      max_drawdown: string;
      average_entry: string;
      executable_cost: string;
      candidate_replay_sha256: string;
      config_sha256: string;
      model_sha256: string;
      runner_git_sha: string;
      promotion_ready: boolean;
      blockers: string[];
    }
  | {
      kind: "event_ml_handoff";
      handoff_id: string;
      status: "ready" | "blocked";
      recommended_action: "promote_to_runtime" | "do_not_promote";
      runtime_score: string;
      candidate_id: string;
      candidate_replay_sha256: string;
      config_sha256: string;
      model_sha256: string;
      runner_git_sha: string;
      blockers: string[];
    }
  | {
      kind: "runtime_parity";
      parity_id: string;
      candidate_id: string;
      candidate_replay_sha256: string;
      symbols: string[];
      strategy_profile: string;
      runtime_score: string;
      dry_run_config_sha256: string;
      expected_live_config_sha256: string;
      live_config_materialized: boolean;
      model_sha256: string;
      runner_git_sha: string;
      recording_sha256: string;
      executable_cost: string;
      average_entry: string;
      max_drawdown: string;
      bankroll: string;
      fees_bps: string;
      slippage_bps: string;
      latency_ms: number;
      max_account_exposure_usd: "5.0";
      strict_parity_ready: boolean;
      blockers: string[];
    }
  | {
      kind: "runtime_recording";
      recording_sha256: string;
      recording_schema_version: string;
      source_start: string;
      source_end: string;
      symbols: string[];
      producer_head_sha: string;
      blockers: string[];
    }
  | {
      kind: "dry_run_report";
      deployment_id: string;
      report_sha256: string;
      dry_run_config_sha256: string;
      deployed_release_sha: string;
      release_manifest_sha256: string;
      runner_sha256: string;
      daemon_boot_id: string;
      source_start: string;
      source_end: string;
      blockers: string[];
    }
  | {
      kind: "paper_runtime";
      deployment_id: string;
      runtime_mode: "paper" | "dry_run";
      desired_state: string;
      observed_state: string;
      active_orders: number;
      open_positions: number;
      strict_parity_ready: boolean;
      blockers: string[];
    };

export type ResearchEvidenceSnapshot = {
  schema_version: "research_evidence_snapshot.v1";
  evidence_id: string;
  collected_at: string;
  reference: ResearchEvidenceReference;
  payload: ResearchEvidencePayload;
};

export type IsolatedCodexInvocation = {
  args: string[];
  env: NodeJS.ProcessEnv;
  workdir: string;
  stdin: string;
  cleanup: () => Promise<void>;
};
```

Extend the existing `AgentTaskCompletion` with required field `research_action: ResearchActionRequest`. Validate every horizon with the shared V2 manifest rules: `0 < prediction_horizon_secs == entry_offset_secs <= market_window_secs`, symbols are normalized/unique, action symbols equal `allowed_symbols`, and 3600/PM1H is rejected.

For backward compatibility, parsers convert a missing action to:

```ts
{ kind: "none", reason: "completion did not request a research action" }
```

Research-only evaluator rule is activated by:

```toml
research_only = true
```

Use an exact receipt allowlist, not a mutation-name denylist. For a `research_only` run, block every receipt whose complete name is not one of the reviewed read-only/model-wrapper names below or one of the deterministic action receipts added in Tasks 2-3, regardless of receipt status:

```ts
const RESEARCH_RECEIPT_ALLOWLIST = new Set([
  "codex_cli__exec",
  "codex_cli__replay-parity",
  "subagent__replay-parity",
  "xai__grok_chat_completions",
  "research_evidence__workflow_run_view",
  "research_evidence__artifact_download",
  "research_evidence__typed_snapshot",
  "research_evidence__paper_runtime",
  "research_action__trace_plan",
  "research_action__execute_plan",
  "research_action__export_event_root_input",
  "research_action__produce_event_root",
  "research_action__run_event_ml",
  "research_action__capture_runtime_recording",
  "research_action__run_candidate_replay",
  "research_action__capture_dry_run_evidence",
  "research_action__run_recorded_parity",
  "research_action__prepare_dry_run_config_pr",
  "research_action__record_research_decision",
  "research_action__record_typed_prior",
  "research_action__create_research_issue",
  "research_action__comment_research_issue",
]);
```

There is no wildcard or prefix escape hatch. Adding a receipt requires a code-reviewed allowlist change and a test proving that it is read-only or one of the bounded deterministic research actions. Unknown names such as `place_live_order`, `withdraw_collateral`, `delete_file`, or an unexpected shell/filesystem tool fail closed even when their receipt status is `failed`.

Additional research-only checks:

- Completion decision `trade` is blocked.
- Parent evidence receipts use only the four exact `research_evidence__` names listed above; their argv/path/schema tests prove read-only temporary-artifact behavior. Any other evidence-loader receipt fails closed.
- `evaluateResearchPreAction` checks completion shape, horizon, decision, and the exact receipt allowlist but does not require an action receipt that cannot exist yet.
- `evaluateResearchPostAction` reruns those checks and additionally requires exactly one receipt whose action ID/kind matches the requested action; `failed`/`blocked` is terminal and `dispatched` is not promotion evidence.
- A completion may never mark itself promotion-ready from JSONL/local harness state.
- Executable replay/runtime parity gates consume typed immutable evidence references collected by the deterministic parent, never model-triggered MCP receipts. References require workflow URL, artifact name, exact head SHA, evidence stage, and an exact matching horizon.

Codex child environment:

```ts
export async function prepareIsolatedResearchCodexInvocation(params: {
  source: NodeJS.ProcessEnv;
  evidence: ResearchEvidenceSnapshot[];
  prompt: string;
}): Promise<IsolatedCodexInvocation>;
```

Deterministic parent evidence loader:

```ts
export const PLOY_RESEARCH_REPOSITORY = "proerror77/ploy" as const;

export type ResearchEvidenceCommandRunner = (
  executable: "gh",
  args: string[],
  options: { cwd: string; env: NodeJS.ProcessEnv },
) => Promise<{ stdout: string; stderr: string; exitCode: number }>;

export type SanitizedPaperRuntimeContext = {
  deployments: Array<{
    deployment_id: string;
    runtime_mode: "paper" | "dry_run";
    desired_state: string;
    observed_state: string;
  }>;
  active_orders: number;
  open_positions: number;
  strict_parity_ready: boolean;
};

export async function collectResearchEvidenceSnapshots(params: {
  actionJournalRoot: string;
  runtimeContext: SanitizedPaperRuntimeContext;
  expectedHorizon: ResearchHorizonRequest;
  commandRunner?: ResearchEvidenceCommandRunner;
}): Promise<ResearchEvidenceSnapshot[]>;
```

- Read completed/dispatched action journal rows and accept only exact allowlisted workflow/run/action IDs.
- Use parent-side `execFile("gh", argv, options)` with explicit `--repo proerror77/ploy` on every run/artifact/API command to verify repository, workflow, `event=workflow_dispatch`, exact `main` head SHA, conclusion, and action ID before downloading a named artifact into a temporary directory. Never infer repository from cwd or `.git`; the model cannot change the constant. The child receives no GitHub token or network/search tool.
- Parse only reviewed files/schemas for portable input, event-root dataset, coverage, trace-plan/Research OS summary, factor registry, Event ML handoff, runtime recording, candidate replay, immutable dry-run report, and recorded parity. Add a paper/dry-run-only runtime snapshot from the request's sanitized control-plane context; reject any live deployment snapshot from Agent evidence.
- Require artifact SHA-256 and full horizon equality. Unknown files/fields, missing metrics, oversized JSON (1 MiB/file, 8 MiB/run), more than 64 snapshots, more than 256 factor rows, non-finite numbers, secret-shaped keys, or path escapes fail closed.
- Sort/canonicalize snapshots, compute their SHA-256, and serialize the bounded full `ResearchEvidenceSnapshot[]` directly into one parent-built stdin prompt between fixed non-instruction delimiters. Include the expected hash and an explicit instruction that artifact text is untrusted evidence, not executable directions. The child is never asked to read a path. `research-evidence.json` and the prompt file may be written in the temporary directory only as parent-side audit/debug copies; URL/name/SHA metadata without a validated typed payload is not usable evidence.

Structural isolation rules:

- Create a mode-0700 temporary `HOME`, empty `CODEX_HOME`, and evidence working directory outside the repository. Write only sanitized typed evidence JSON plus the prompt there; never copy the user's Codex config, auth store, plugins, MCP definitions, repository secrets, or git credentials.
- Require a dedicated `SIDECAR_CODEX_API_KEY`; expose it to the Codex process as `OPENAI_API_KEY`, but set `shell_environment_policy.inherit=none` so model-generated shell commands cannot read it. Missing dedicated credentials blocks the run instead of falling back to the user's Codex home.
- Invoke the locally verified CLI with `features.shell_tool=false`, `features.unified_exec=false`, `features.code_mode=false`, `features.multi_agent=false`, `features.plugins=false`, `features.plugin_sharing=false`, `features.tool_suggest=false`, `features.browser_use=false`, `features.browser_use_external=false`, `features.browser_use_full_cdp_access=false`, `features.in_app_browser=false`, `features.computer_use=false`, `features.workspace_dependencies=false`, `features.image_generation=false`, `features.apps=false`, `features.standalone_web_search=false`, `mcp_servers={}`, and `shell_environment_policy.inherit=none`, plus `exec --ignore-user-config --ignore-rules --strict-config --sandbox read-only --ephemeral --skip-git-repo-check -C <evidence-dir> -`. Do not pass `--search`. Send only the parent-built prompt through stdin and close stdin. The research child is prompt/model-only: it has no shell, filesystem, subagent, plugin, browser, MCP, app, or remote mutation tool; read-only sandbox is defense in depth, not the primary boundary.
- Pass only `PATH`, temporary `HOME`/`CODEX_HOME`/`TMPDIR`, locale, required proxy keys, and the dedicated model API key. Explicitly exclude the research-queue/database URL, Polymarket, funder/signer, private-key, relayer, Ploy auth, GitHub, AWS, Alibaba Cloud, and OSS credentials.
- Remove the whole temporary tree in `finally`. The receipt allowlist remains a second audit layer, not the mechanism preventing side effects.
- Parse Codex JSONL with an exact output-item allowlist. Any `command_execution`, `file_change`, shell/unified-exec, MCP, web/search, browser, computer-use, or unknown tool item fails the run and emits a forbidden receipt even when it lacks `tool_name` or reports failure. Only assistant/reasoning/usage/final structured-output items are accepted.

Step 1: Add failing self-tests.

```ts
research_only_blocks_every_receipt_outside_exact_allowlist
research_only_blocks_unknown_attempt_even_when_tool_status_is_failed
research_only_allows_only_model_wrappers_and_deterministic_action_receipts
research_only_blocks_place_live_order_withdraw_collateral_and_delete_file
research_only_blocks_trade_decision
missing_research_action_defaults_to_none
pre_action_evaluation_does_not_require_future_action_receipt
post_action_evaluation_requires_matching_action_id_kind_and_receipt
action_horizon_rejects_pm1h_symbol_or_window_mismatch
codex_child_uses_empty_home_no_mcp_and_dedicated_key_only
research_only_codex_disables_shell_exec_browser_mcp_and_search
codex_parser_fails_closed_on_command_execution_without_tool_name
codex_parser_fails_closed_on_file_change_or_unknown_tool_item
canonical_typed_evidence_is_embedded_in_stdin_and_hash_checked
codex_child_disables_multi_agent_plugins_suggestions_and_all_browser_variants
no_tool_probe_emits_only_allowed_jsonl_items
parent_loader_downloads_only_exact_action_run_and_validates_sha_horizon
parent_loader_uses_fixed_repo_without_git_checkout_context
parent_loader_rejects_metadata_only_oversized_secret_shaped_or_unknown_evidence
parent_loader_emits_typed_portable_dataset_coverage_trace_registry_handoff_candidate_recording_dryrun_parity_and_paper_snapshots
```

Step 2: Run RED.

```bash
npm run test --prefix ploy-sidecar
```

Expected RED result: evaluator checks only three mutation names under the approval gate, completion has no typed action, and Codex inherits the full environment.

Step 3: Implement the contract and evaluator.

- Define the shared internal types once in `run-recorder.ts` or a new small `research-actions.ts` type module introduced by Task 2; do not duplicate the union in Codex/Grok parsers.
- Preserve NBA `grok_decision` behavior when `research_only=false`.
- Record the exact non-allowlisted receipt names in the evaluator detail for auditability.
- Keep the current CLI adapter for non-research profiles. Only the research-only invocation sets the no-tool feature overrides and strict item parser.

Step 4: Verify.

```bash
npm run contracts:check --prefix ploy-sidecar
npm run test --prefix ploy-sidecar
npm run build --prefix ploy-sidecar
npm audit --omit=dev --audit-level=moderate --prefix ploy-sidecar
rtk git diff --check
```

Step 5: Commit.

```bash
git add ploy-sidecar/src/runtime/run-recorder.ts \
  ploy-sidecar/src/runtime/evaluator.ts \
  ploy-sidecar/src/runtime/codex-cli.ts \
  ploy-sidecar/src/runtime/grok.ts \
  ploy-sidecar/src/runtime/research-evidence.ts \
  ploy-sidecar/package.json
git commit -m "fix(sidecar): enforce research-only agent contract"
```

---

### Task 2: Add a deterministic allowlisted research action adapter

Files:

- Add `ploy-sidecar/src/runtime/research-actions.ts`.
- Add `ploy-sidecar/src/runtime/research-action-journal.ts`.
- Modify `ploy-sidecar/package.json` to run its self-test.
- Modify `.github/workflows/research-trace-plan.yml`.
- Modify `.github/workflows/research-manager-execute-plan.yml`.
- Modify `.github/workflows/research-snapshot.yml`.
- Modify `.github/workflows/event-root-dataset-producer.yml` added by the prerequisite horizon-safe research plan.
- Modify `.github/workflows/event-ml-rolling-evidence.yml`.
- Modify `.github/workflows/event-ml-config-pr.yml` added by the prerequisite horizon-safe research plan.
- Add `.github/workflows/runtime-recording-export.yml` as the only fixed-path research-host recording exporter.
- Add `.github/workflows/trade-runtime-evidence-snapshot.yml` as the only read-only trade-host dry-run report/config/release snapshotter.
- Modify `.github/workflows/runtime-candidate-replay.yml`.
- Modify `.github/workflows/recorded-replay-parity.yml`.
- Add `.github/workflows/research-record-agent-evidence.yml`.
- Modify `crates/ploy-research/examples/research_trace_plan.rs`.
- Modify `scripts/research_manager_execute_plan.py`.
- Add `scripts/build_live_config_candidate.py` and `tests/test_build_live_config_candidate.py` as a pure deterministic transform/hash helper; it never writes the checkout.
- Modify `tests/test_persist_research_trace_contract.py`.
- Modify `tests/test_research_manager_execute_plan.py`.
- Modify `tests/workflow_security.rs`.

Receipt:

```ts
export type ResearchActionReceipt = {
  action_id: string;
  kind: ResearchActionRequest["kind"];
  status: "none" | "prepared" | "dispatched" | "recovered" | "blocked" | "failed";
  horizon?: ResearchHorizonRequest;
  workflow?:
    | "research-trace-plan.yml"
    | "research-manager-execute-plan.yml"
    | "research-snapshot.yml"
    | "event-root-dataset-producer.yml"
    | "event-ml-rolling-evidence.yml"
    | "event-ml-config-pr.yml"
    | "runtime-recording-export.yml"
    | "trade-runtime-evidence-snapshot.yml"
    | "runtime-candidate-replay.yml"
    | "recorded-replay-parity.yml"
    | "research-record-agent-evidence.yml";
  run_url?: string;
  issue_url?: string;
  head_sha?: string;
  reason?: string;
};
```

Execution entrypoint:

```ts
export async function executeResearchAction(params: {
  action: ResearchActionRequest;
  evidence: ResearchEvidenceSnapshot[];
  runId: string;
  queueAttempt: number;
  repoRoot: string;
  journalRoot: string;
  commandRunner?: ResearchCommandRunner;
  environment?: NodeJS.ProcessEnv;
}): Promise<ResearchActionReceipt>;
```

Hard-coded mapping:

| Action | Command surface | Mandatory gate |
| --- | --- | --- |
| `none` | no command | none |
| `trace_plan` | `gh workflow run research-trace-plan.yml --ref main` | `SIDECAR_ALLOW_RESEARCH_WORKFLOWS=true` |
| `execute_plan` | `gh workflow run research-manager-execute-plan.yml --ref main`, mode fixed to `dry_run` | `SIDECAR_ALLOW_RESEARCH_WORKFLOWS=true` |
| `export_event_root_input` | `gh workflow run research-snapshot.yml --ref main` with portable export required | both workflow gate and `SIDECAR_ALLOW_RESEARCH_EXECUTION=true` |
| `produce_event_root` | `gh workflow run event-root-dataset-producer.yml --ref main` | `SIDECAR_ALLOW_RESEARCH_WORKFLOWS=true` |
| `run_event_ml` | `gh workflow run event-ml-rolling-evidence.yml --ref main` with config/PR flags forced false | both workflow gate and `SIDECAR_ALLOW_RESEARCH_EXECUTION=true` |
| `capture_runtime_recording` | `gh workflow run runtime-recording-export.yml --ref main` | both workflow gate and `SIDECAR_ALLOW_RESEARCH_EXECUTION=true` |
| `run_candidate_replay` | `gh workflow run runtime-candidate-replay.yml --ref main` | both workflow gate and `SIDECAR_ALLOW_RESEARCH_EXECUTION=true` |
| `capture_dry_run_evidence` | `gh workflow run trade-runtime-evidence-snapshot.yml --ref main` | both workflow gate and `SIDECAR_ALLOW_RESEARCH_EXECUTION=true` |
| `run_recorded_parity` | `gh workflow run recorded-replay-parity.yml --ref main` | both workflow gate and `SIDECAR_ALLOW_RESEARCH_EXECUTION=true` |
| `prepare_dry_run_config_pr` | `gh workflow run event-ml-config-pr.yml --ref main` | `SIDECAR_ALLOW_RESEARCH_CONFIG_PR=true` plus exact ready evidence |
| `record_research_decision` / `record_typed_prior` | `gh workflow run research-record-agent-evidence.yml --ref main` | `SIDECAR_ALLOW_RESEARCH_EVIDENCE=true` |
| `create_research_issue` | `gh issue create` with fixed `research` label | `SIDECAR_ALLOW_RESEARCH_ISSUES=true` |
| `comment_research_issue` | `gh issue comment <number>` | `SIDECAR_ALLOW_RESEARCH_ISSUES=true` |

Adapter rules:

- The model never supplies `workflow`, `--ref`, repository, ACK, executable, or arbitrary argv.
- Git ref is always `main`; every `gh workflow`, `gh run`, `gh api`, and `gh issue` argv includes `--repo proerror77/ploy` (or the equivalent fixed `repos/proerror77/ploy/...` API path) and works from an evidence directory with no `.git` checkout.
- Serialize the complete validated horizon as canonical sorted JSON. Pass it as `horizon_json` (or `expected_horizon_json` for a consumer) plus `orchestrator_action_id` to every workflow. `research_trace_plan.v2`, its downloaded artifact, the manager executor, producer provenance, and Event ML source manifest must all match it byte-for-byte after canonicalization.
- `execute_plan` is permanently dry-run in the Agent adapter; the model cannot supply mode or ACK and the manager never dispatches a child workflow from this action. Executable phases use the explicit typed Event ML, candidate replay, and recorded parity actions above.
- Evidence stage must be one of the typed values; `live_candidate` is not accepted.
- `limit` is 1-100, `chain_remaining` is 0-3, `stake_usd` is positive and at most 100, symbols match `^[A-Z0-9:_-]+$`, issue number is positive, title is at most 200 characters, and issue body is at most 20,000 characters.
- Decision/prior `evidence_ids` are non-empty, unique, and must resolve to the exact validated snapshots supplied to that completion with the same horizon. Rationale/reason strings are bounded and secret-key patterns are rejected.
- Use `execFile`, never a shell string.
- Compute `action_id = sha256(run_id + queue_attempt + canonical_action_json)`. Before any external mutation, atomically create a mode-0600 journal record with status `prepared`; if that write fails, execute no command.
- Workflows accept `orchestrator_action_id`, use it in exact `run-name`, concurrency, provenance, and summaries. After dispatch, poll workflow runs and match exact display title/action ID, workflow, event, repository, `main` head SHA, and creation time. Never select a generic latest run.
- On restart with a `prepared` journal, recover only an exact matching run/issue/comment. If none is provable, return blocked `ambiguous_external_outcome` and never redispatch automatically. This chooses at-most-once behavior over duplicate research mutations.
- Research issues include hidden marker `<!-- ploy-orchestrator-action:<action_id> -->`; comments use the same marker and scan existing comments before posting.
- `export_event_root_input` passes validated ISO dates, exact symbol set, full horizon, and action ID to `research-snapshot.yml`; it cannot supply SQL, DB URL, host, artifact path, or SSH options. The protected research workflow is the only PostgreSQL-to-portable producer.
- `produce_event_root` requires its source run/artifact to resolve to one validated `portable_input` snapshot supplied to this completion; it passes that exact reference/hash to the hosted producer. `run_event_ml` similarly requires one validated `event_root_dataset` snapshot. A journal URL or artifact name without the typed payload cannot advance either action.
- `run_event_ml` forces `create_handoff_issue=false`, rejects/removes `create_config_pr`, and forces all deployment flags false. Training cannot mutate config.
- `capture_runtime_recording` accepts only validated dates, exact horizon/symbols, and action ID. `capture_dry_run_evidence` accepts only an exact validated candidate-replay run/artifact/action ID. Neither action accepts a host, path, deployment ID, token, SSH option, config, report URL, or command; the workflows select those through reviewed horizon maps.
- `prepare_dry_run_config_pr` is reachable only when the supplied parent evidence contains an exact ready Event ML handoff and matching promotion-ready candidate replay from successful same-main-SHA workflows, with finite executable cost/average entry/max drawdown and equal horizon/runtime-score/config/model/runner hashes. The adapter passes only run/artifact IDs, canonical horizon, and action ID to the dedicated config-PR workflow; target path is selected inside that workflow. It can open a review-required dry-run PR only and never merge or deploy.
- Candidate replay and recorded parity workflows accept `orchestrator_action_id` plus the full expected horizon, use exact run-name/concurrency identity, reject artifact/head-SHA/horizon mismatch, and expose no config-PR/deploy input. Candidate replay additionally requires one exact typed `runtime_recording` artifact; recorded parity requires one exact candidate-replay artifact and one exact typed `dry_run_report` artifact. A workflow may never select a generic latest artifact or read a mutable report endpoint during the comparison job.
- `recorded-replay-parity.yml` emits the generated operator-contract `RecordedRuntimeParityV2` exactly: source workflow/run/main SHA; candidate ID/replay hash; symbols/profile/runtime score/horizon; dry-run config/model/runner/recording hashes; deterministic `expected_live_config_sha256` and `live_config_materialized`; executable cost, average entry, maximum drawdown, bankroll, fee/slippage/latency assumptions; fixed maximum account exposure `5.0`; strict parity boolean; and blockers. `build_live_config_candidate.py` maps the fixed dry-run path to a fixed live path by changing only reviewed runtime-mode/live-template fields, writes only a caller-supplied temporary output, and returns its hash; arbitrary paths/fields fail. Before the config PR merges, parity sets `live_config_materialized=false` and may be used only to prepare that PR. After merge, the workflow must rerun replay/dry-run parity from the new exact main/release SHA; only exact on-disk live bytes set `live_config_materialized=true`. The workflow uploads retained parity/candidate evidence and can never commit, label itself human-approved, or mark live-ready.
- The record-evidence workflow validates and canonicalizes either `research_decision.v1` or the existing `research_manager_typed_prior.v1`, caps it at 20 KiB, rejects promote/live/deploy decisions and unknown mutation types, and uploads one immutable `research-agent-evidence-<action_id>` artifact. It never writes config, factor registry, or deployment state; a later run must load the artifact through the deterministic evidence loader.
- Typed priors allow at most 16 mutations and 32 avoid rows. Mutation type is exactly one of `add_feature_gate`, `add_capacity_gate`, `add_near_strike_interaction`, `add_spread_penalty`, `replace_denominator`, `clip_or_squash`, `change_time_window`, `invert_or_contrarian`, or `remove_component`; every numeric field must be finite and the existing AutoFactor compiler must accept the draft before the workflow publishes it.
- With gates disabled, return `blocked` and execute no command.
- Local tests use a fake command runner and never invoke `gh`.

Dual-host replay/parity execution contract:

- These four workflows land with repository-owned `DUAL_HOST_EVIDENCE_READY=false`; no dispatch input or secret can override it. The dual-host packaging Task 9 flips it only after both role installs, immutable release verification, read-only evidence credentials, and the complete workflow-security matrix pass.
- `runtime-recording-export.yml` is the only Tango bridge. It runs under environment `tango-1-1-evidence`, uses fixed `TANGO_1_1_HOST`/SSH/known-host secrets, selects a read-only canonical recording path from a committed horizon map, and copies bytes into the GitHub runner without executing a strategy. Its retained `runtime-recording-<action_id>` artifact contains the recording plus `runtime-recording.v1.json`: repository, workflow/run/action ID, producer head SHA, canonical horizon/symbols/time range, format/schema, byte length, and recording SHA-256. Host/path/SSH inputs are absent.
- `trade-runtime-evidence-snapshot.yml` runs under environment `ploy-trade-1-evidence` with only `PLOY_TRADE_1_HOST`, pinned SSH/known-host values, and a read-only report token. It selects the dry-run deployment/config from the same committed horizon map, reads `/opt/ploy/current/release.json`, hashes the exact release-pinned runner and config, and obtains one bounded deployment/time-filtered dry-run report without pausing/resuming/reloading any service. Its retained `trade-runtime-evidence-<action_id>` artifact contains the config/report plus `dry-run-report.v1.json`: workflow/run/action/head SHA, deployment/time range, report/config SHA-256, deployed release SHA, release-manifest SHA-256, runner SHA-256, daemon boot ID, and blockers.
- `runtime-candidate-replay.yml` runs only on the protected `ploy-trade-1-evidence` lane. A hosted preflight resolves the exact successful Event ML candidate artifact and `runtime-recording-export.yml` artifact by repository/workflow/run/artifact ID, validates main-head/action/horizon provenance, and recomputes every content hash. It transfers only those verified bytes to a mode-0700 temporary directory owned by an unprivileged `ploy-evidence` account, then executes `/opt/ploy/releases/<candidate-runner-head-sha>/bin/ploy-runner` in explicit replay mode with dotenv disabled and no wallet, daemon-admin, live-gate, GitHub, cloud, or database credential. Before execution it requires that release's `release.json`, runner SHA, candidate config SHA, recording SHA, and runner Git SHA all match the input manifests. It writes nowhere under `/opt/ploy/current`, never controls systemd/deployments, uploads replay output plus `runtime-candidate-replay.v2.json` with the complete source/hash chain, and deletes the temporary directory on every exit.
- `recorded-replay-parity.yml` performs comparison on a hosted GitHub runner and has no host or SSH secret. It downloads only the exact successful candidate-replay and `trade-runtime-evidence-snapshot.yml` artifacts named in the action, verifies repository/workflow/run/action/main SHA, canonical horizon, artifact/content hashes, deployed release/runner/config identity, recording hash, deployment/time overlap, and report hash, then emits `RecordedRuntimeParityV2`. It never rereads a mutable host endpoint and never accepts a caller-supplied file/path/hash in place of an artifact.
- The two replay/parity workflows contain no `TANGO_1_1_*`, `tango-1-1-build-only`, `/opt/ploy/data/recordings`, or Tango SSH reference. Only the fixed recording-export workflow may reference Tango. Workflow tests scan complete YAML and shell blocks for this separation.
- A hash/source mismatch, absent retained artifact, stale/non-main runner release, time-window mismatch, missing report rows, or unavailable host makes the receipt blocked. There is no fallback to a latest run, a research-host runner/config, a mutable on-host recording path, or a local JSON file.

Step 1: Add failing self-tests in the new module.

```ts
workflow_name_ref_and_execute_ack_cannot_come_from_model
every_gh_action_uses_fixed_repo_and_runs_without_git_checkout
disabled_workflow_gate_runs_no_command
manager_execute_mode_cannot_be_requested_by_model
trace_plan_builds_exact_allowlisted_argv
execute_plan_dry_run_cannot_dispatch_deploy_workflow
producer_and_event_ml_actions_are_reachable_with_exact_horizon
postgres_export_action_is_reachable_without_model_controlled_db_or_ssh
portable_and_dataset_actions_require_exact_typed_parent_artifacts
recording_and_dryrun_capture_actions_have_no_model_controlled_host_path_or_credential
candidate_replay_and_recorded_parity_are_reachable_with_exact_horizon
candidate_replay_requires_exact_recording_candidate_release_runner_and_config_hashes
recorded_parity_consumes_only_exact_immutable_replay_and_dryrun_report_artifacts
replay_and_parity_workflows_have_no_tango_host_or_secret_reference
dual_host_evidence_workflows_remain_hard_disabled_until_packaging_acceptance
recorded_parity_emits_exact_v2_contract_with_live_gate_metrics_but_no_approval
recorded_parity_computes_deterministic_live_config_hash_without_repo_mutation
post_config_merge_parity_rerun_is_required_for_live_config_materialized_true
ready_handoff_can_prepare_only_dry_run_config_pr
blocked_or_mismatched_handoff_cannot_dispatch_config_pr_workflow
manager_plan_action_is_always_dry_run_and_never_dispatches_children
decision_and_typed_prior_write_only_validated_immutable_artifacts
typed_prior_rejects_unknown_nonfinite_or_oversized_mutations
pm15d_action_cannot_consume_pm5d_plan_or_dataset_artifact
invalid_stage_horizon_symbol_limit_and_issue_input_fail_closed
prepared_journal_is_written_before_external_command
crash_after_dispatch_recovers_exact_action_id_without_redispatch
ambiguous_prepared_action_blocks_without_redispatch
dispatch_without_exact_action_id_run_url_is_failed
issue_body_is_passed_as_execfile_argument_not_shell
event_ml_action_cannot_enable_config_pr_or_deployment
config_pr_action_cannot_choose_path_merge_deploy_or_live_mode
research_actions_cannot_dispatch_live_canary_or_access_hmac_approval_environment
```

Step 2: Run RED.

```bash
npx tsx ploy-sidecar/src/runtime/research-actions.ts
```

Expected RED result: the module does not exist.

Step 3: Implement the narrow adapter.

- Keep this independent of `self-modification.ts`; do not expose its deploy workflow allowlist.
- Assert `promote-live-config.yml`, `live-canary-gate.yml`, `release-aliyun.yml`, `PLOY_LIVE_GATE_HMAC_KEY`, approval creation, generic resume, and every production environment name are absent from the research action mapping and child environment.
- It is acceptable to duplicate a small `execFile` wrapper rather than create a generic workflow engine.
- Never treat a dispatched workflow as completed evidence; receipt status is `dispatched` only.
- Upgrade the trace-plan artifact to `research_trace_plan.v2` with its full horizon and make the manager reject V1 or horizon mismatch for this profile. Preserve old V1 artifacts as diagnostic history only.
- Extend manager tests so artifact production, coverage, attribution, feature governance, bounded search, walk-forward, candidate replay, and recorded parity are either mapped to a horizon-aware allowlisted workflow or explicitly blocked before dispatch.
- Keep manager execution as planning evidence only for this profile. Coverage through walk-forward is owned by the explicit Event ML action; candidate replay and recorded parity each have their own typed action, so no generic downstream workflow dispatch remains reachable.

Step 4: Verify.

```bash
npx tsx ploy-sidecar/src/runtime/research-actions.ts
npm run test --prefix ploy-sidecar
npm run build --prefix ploy-sidecar
rtk pytest tests/test_build_live_config_candidate.py -q
ruby -e 'require "yaml"; ARGV.each { |p| YAML.load_file(p) }' \
  .github/workflows/runtime-recording-export.yml \
  .github/workflows/trade-runtime-evidence-snapshot.yml \
  .github/workflows/runtime-candidate-replay.yml \
  .github/workflows/recorded-replay-parity.yml
rtk git diff --check
```

Step 5: Commit.

```bash
git add ploy-sidecar/src/runtime/research-actions.ts \
  ploy-sidecar/src/runtime/research-action-journal.ts \
  ploy-sidecar/package.json \
  .github/workflows/research-trace-plan.yml \
  .github/workflows/research-manager-execute-plan.yml \
  .github/workflows/research-snapshot.yml \
  .github/workflows/event-root-dataset-producer.yml \
  .github/workflows/event-ml-rolling-evidence.yml \
  .github/workflows/event-ml-config-pr.yml \
  .github/workflows/runtime-recording-export.yml \
  .github/workflows/trade-runtime-evidence-snapshot.yml \
  .github/workflows/runtime-candidate-replay.yml \
  .github/workflows/recorded-replay-parity.yml \
  .github/workflows/research-record-agent-evidence.yml \
  crates/ploy-research/examples/research_trace_plan.rs \
  scripts/research_manager_execute_plan.py \
  scripts/build_live_config_candidate.py \
  tests/test_build_live_config_candidate.py \
  tests/test_persist_research_trace_contract.py \
  tests/test_research_manager_execute_plan.py \
  tests/workflow_security.rs
git commit -m "feat(sidecar): add allowlisted research actions"
```

---

### Task 3: Integrate one research action into a cross-host durable queue/run record

Files:

- Modify `ploy-sidecar/src/index.ts`.
- Modify `ploy-sidecar/src/runtime/run-recorder.ts`.
- Modify `ploy-sidecar/src/runtime/evaluator.ts`.
- Modify `ploy-sidecar/src/runtime/run-requests.ts` only if the typed completion must be preserved through retry fixtures.
- Modify `ploy-sidecar/src/runtime/research-evidence.ts` to remove Task 1's temporary local `SanitizedPaperRuntimeContext` declaration and import the generated operator-contract type.
- Add `ploy-sidecar/src/runtime/research-request-store.ts` and the production `pg` dependency/lockfile entries.
- Modify `crates/ploy-operator-contracts/src/diagnostics.rs`, `src/lib.rs`, `src/schemas.rs`, and regenerate the Agent request/envelope schemas and both TypeScript contract files.
- Add `migrations/051_research_agent_queue.sql` (after V2 Task 6 reserves migration 050 for live approvals).
- Add `crates/ploy-daemon-host/src/agent_requests.rs`.
- Modify `crates/ploy-daemon-host/src/lib.rs`, `src/config.rs`, `src/http.rs`, `src/runtime.rs`, and `Cargo.toml`.
- Add `crates/ploy-daemon-host/tests/research_agent_queue_postgres.rs` and modify `.github/workflows/test.yml` to run it against the existing PostgreSQL service.

Execution order for one queued research-only run:

```text
claim durable request
-> validate admission/turn budget
-> collect read-only runtime context
-> recover exact prior action journal rows and build validated typed evidence snapshots
-> run bounded read-only Codex/Grok completion
-> run evaluateResearchPreAction(completion, receipts, evidence)
-> if still allowed, execute at most one typed research action
-> append action receipt to tool calls and output summary
-> run evaluateResearchPostAction(completion, action receipt, evidence)
-> record terminal run
-> checkpoint queue item
```

Cross-host queue contract:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SanitizedRuntimeMode { Paper, DryRun }

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SanitizedPaperDeployment {
    pub deployment_id: String,
    pub runtime_mode: SanitizedRuntimeMode,
    pub desired_state: String,
    pub observed_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SanitizedPaperRuntimeContext {
    pub deployments: Vec<SanitizedPaperDeployment>,
    pub active_orders: u64,
    pub open_positions: u64,
    pub strict_parity_ready: bool,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentRunQueueEnvelope {
    pub schema_version: String,
    pub request_id: String,
    pub request: AgentRunCreateRequest,
    pub runtime_context: SanitizedPaperRuntimeContext,
    pub enqueued_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunAttemptStatus { InProgress, Completed, Failed, Blocked }

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResearchActionReceiptSnapshot {
    pub action_id: String,
    pub kind: String,
    pub status: String,
    pub workflow: Option<String>,
    pub run_url: Option<String>,
    pub head_sha: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentRunAttempt {
    pub request_id: String,
    pub attempt: u32,
    pub lease_owner: String,
    pub status: AgentRunAttemptStatus,
    pub action_id: Option<String>,
    pub action_receipt: Option<ResearchActionReceiptSnapshot>,
    pub run_record: Option<AgentRunRecord>,
    pub run_record_sha256: Option<String>,
    pub retry_reason: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}
```

- Reuse `POST /api/agent/runs` as the ingress on the trade/control plane. In production PostgreSQL mode it inserts an `AgentRunQueueEnvelope` into `ploy_research_agent_requests` instead of appending a host-local JSONL file.
- The envelope contains the existing typed `AgentRunCreateRequest` plus daemon-produced `SanitizedPaperRuntimeContext`. The client cannot supply that context. Include only paper/dry-run deployment IDs/states and aggregate order/position counts; any live deployment detail is reduced to a blocker and never serialized to the research queue.
- Task 3 promotes Task 1's temporary internal `SanitizedPaperRuntimeContext` shape into `ploy-operator-contracts` and replaces the Sidecar-local declaration with the generated import. Rust and TypeScript use one wire schema; no duplicate context type remains.
- The research-host Sidecar requires `SIDECAR_RUN_REQUEST_STORE=postgres` and a dedicated least-privilege `SIDECAR_RESEARCH_QUEUE_DATABASE_URL`. It claims rows in one transaction with `FOR UPDATE SKIP LOCKED`, a bounded lease owner/expiry, incremented attempt, and deterministic request ID. Completion/requeue/action-receipt updates use compare-and-swap on request ID, attempt, and lease owner.
- The database role may select/claim/update only `ploy_research_agent_requests` and append/read its own bounded run results; it has no privileges on canonical trading state, wallet/account data, migrations, or market tables. Credentials live only in `/opt/ploy/env/research-agent.env` and are never forwarded to the Codex child.
- JSONL remains an explicit `SIDECAR_RUN_REQUEST_STORE=file` local-development/test adapter. The deployed research profile rejects file mode, absent PostgreSQL, expired/ambiguous leases, or schema/version mismatch; two hosts never rely on a shared filesystem.
- Production `POST /api/agent/runs` requires an `Idempotency-Key` header: 16-128 printable non-whitespace ASCII characters, scoped with the authenticated principal. Store its SHA-256 plus canonical request hash under a unique constraint. Repeating the same key+payload returns the existing request; the same key with different payload is HTTP 409. File-mode local tests may derive a key from canonical request JSON, but production never does.
- Migration 051 creates two tables. `ploy_research_agent_requests` stores request ID, scoped idempotency hash, canonical request hash/JSON, sanitized context JSON, aggregate request status, `next_attempt`, current lease owner/expiry, and timestamps. `ploy_research_agent_attempts` has primary key `(request_id, attempt)`, immutable claimed envelope, status, action ID/receipt, full typed `AgentRunRecord` JSON, record SHA-256, error/retry reason, and started/finished timestamps.
- An attempt in `completed`, `failed`, or `blocked` is immutable. Retry never edits it: a CAS on the request row may move aggregate status back to `queued`, increment `next_attempt`, and later insert a new attempt only when the bounded retry policy permits. A completed request is terminal; crash recovery may reclaim only an expired nonterminal attempt lease. This removes the contradiction between immutable evidence and retry.
- Production `GET /api/agent/runs` and exact-run lookup read PostgreSQL request plus attempt rows, verify each record hash, and return the full generated `AgentRunRecord`/attempt history contract. JSONL readers are used only by file mode. Missing/corrupt attempt JSON fails closed instead of returning a partial success.

Production Sidecar runtime contract:

- Add exact (no range) production dependencies `@openai/codex` and `pg`, locked with npm integrity metadata. Set `SIDECAR_CODEX_COMMAND=/opt/ploy/current/sidecar/node_modules/.bin/codex`; production startup resolves that file inside the active release, compares `codex --version` with the locked package version, and runs the strict no-tool prompt-input probe before claiming a request. It never falls back to a host-global `codex`.
- Require `SIDECAR_RUNTIME_PROFILE=polymarket_research_only`, `SIDECAR_AGENT_ENGINE=codex`, and the fixed `PLOY_RESEARCH_REPOSITORY`. In this profile `index.ts` runs only the awaited PostgreSQL request poll loop. It never schedules or calls the legacy NBA `runScanCycle`, Grok scan, self-modification/deploy loop, or any non-queue periodic scan; selecting another engine/profile is a startup error.
- Local/default developer behavior may retain the NBA profile, but profile selection is explicit and tests inject fake Codex/DB/GitHub runners. The deployed systemd unit fixes the research-only values after its environment-file directive so the env file cannot re-enable NBA or a host-global command.

Rules:

- Never execute an action before the preliminary evaluator proves no forbidden tool receipt/decision.
- Evidence collection is a deterministic parent step before model invocation. Missing/failed/ambiguous prior action evidence becomes a typed blocker; it is never represented by a bare URL or silently omitted.
- The request's daemon-supplied sanitized paper context is the only direct runtime context available on the research host. Later immutable dry-run/parity workflow artifacts may enrich it; the Sidecar never opens a trade-host filesystem or receives a daemon admin credential.
- Preliminary evaluation never requires the future action receipt. Only post-action evaluation matches action ID/kind/horizon/status and marks a failed/blocked/ambiguous receipt terminal.
- One action consumes one turn. If no turn remains, block without dispatch.
- At most one action per run/attempt.
- Use deterministic receipt names:

```text
research_action__trace_plan
research_action__execute_plan
research_action__export_event_root_input
research_action__produce_event_root
research_action__run_event_ml
research_action__capture_runtime_recording
research_action__run_candidate_replay
research_action__capture_dry_run_evidence
research_action__run_recorded_parity
research_action__prepare_dry_run_config_pr
research_action__record_research_decision
research_action__record_typed_prior
research_action__create_research_issue
research_action__comment_research_issue
```

- Map `dispatched` to tool status `called`; map `failed/blocked` to `failed`; `none` produces no mutating receipt.
- Persist `research_action_receipt` in `output_summary`.
- Persist the deterministic action journal record before dispatch. Existing `run_id:queue_attempt` plus `action_id` recovery is authoritative; a terminal or ambiguous attempt is never redispatched.
- A successful workflow dispatch does not change promotion status; the next run must inspect its artifact/Research OS trace.
- If action fails after the model completion, terminal status is blocked/failed and the queue retry policy applies without duplicate dispatch for the same terminal attempt.

Step 1: Add failing tests.

```ts
forbidden_preliminary_evaluation_prevents_research_action
research_action_consumes_one_turn
one_run_executes_at_most_one_action
action_receipt_is_persisted_in_output_summary
dispatched_workflow_does_not_mark_promotion_ready
terminal_attempt_is_not_redispatched_after_restart
pre_action_accepts_valid_non_none_request_without_action_receipt
post_action_blocks_missing_or_mismatched_action_receipt
crash_between_dispatch_and_run_record_uses_action_journal_recovery
trade_ingress_writes_postgres_queue_with_daemon_sanitized_paper_context
postgres_claim_is_single_consumer_and_lease_recovery_is_bounded
production_enqueue_requires_scoped_idempotency_key_and_conflicts_on_payload_change
failed_attempt_is_immutable_and_retry_creates_next_attempt_row
postgres_get_agent_runs_returns_full_hash_verified_agent_run_records
research_queue_role_cannot_read_trading_or_market_tables
production_sidecar_rejects_file_queue_or_live_runtime_context
generated_runtime_context_schema_rejects_live_mode
generated_runtime_context_types_compile_in_research_evidence_loader_without_local_duplicate
cross_host_sidecar_consumes_request_without_shared_filesystem_or_daemon_token
production_uses_release_pinned_official_codex_binary_and_rejects_global_fallback
polymarket_research_only_profile_never_runs_nba_grok_or_self_modification_scan
production_profile_requires_fixed_repo_codex_engine_and_postgres_queue
```

Use dependency injection for `executeResearchAction`; do not call GitHub.

Step 2: Run RED.

```bash
SIDECAR_SELF_TEST=true npx tsx ploy-sidecar/src/index.ts
npx tsx ploy-sidecar/src/runtime/run-requests.ts
rtk cargo test -p ploy-daemon-host research_agent_queue --lib
```

Expected RED result: completion action is ignored and no receipt reaches the evaluator/recorder.

Step 3: Implement the two-phase evaluation.

- Add an `actionExecutor` parameter to a small exported `runQueuedStrategyRequestForTest` wrapper or factor the queued-run core into a testable function; do not expose the daemon/admin client.
- Preserve single-flight `runAwaitedPollLoop` behavior.
- Keep SQL dynamic so local compilation needs no database. Run the ignored real PostgreSQL lease/idempotency test explicitly in CI; local tests use fake stores and never start PostgreSQL.

Step 4: Verify.

```bash
npm run contracts:check --prefix ploy-sidecar
npm run test --prefix ploy-sidecar
npm run build --prefix ploy-sidecar
cargo run -p ploy-operator-contracts --example export_schemas
node scripts/export_operator_contract_types.mjs
rtk cargo test -p ploy-daemon-host research_agent_queue --lib
rtk cargo test -p ploy-operator-contracts agent_run
rtk git diff --check
```

Step 5: Commit.

```bash
git add ploy-sidecar/src/index.ts \
  ploy-sidecar/src/runtime/run-recorder.ts \
  ploy-sidecar/src/runtime/evaluator.ts \
  ploy-sidecar/src/runtime/run-requests.ts \
  ploy-sidecar/src/runtime/research-evidence.ts \
  ploy-sidecar/src/runtime/research-request-store.ts \
  ploy-sidecar/package.json ploy-sidecar/package-lock.json \
  crates/ploy-operator-contracts/src/diagnostics.rs \
  crates/ploy-operator-contracts/src/lib.rs \
  crates/ploy-operator-contracts/src/schemas.rs \
  contracts/schemas/agent-run-create-request.schema.json \
  contracts/schemas/agent-run-queue-envelope.schema.json \
  contracts/schemas/agent-run-attempt.schema.json \
  ploy-sidecar/src/contracts/operator-contracts.ts \
  ploy-frontend/src/types/operator-contracts.ts \
  crates/ploy-daemon-host/src/agent_requests.rs \
  crates/ploy-daemon-host/src/lib.rs \
  crates/ploy-daemon-host/src/config.rs \
  crates/ploy-daemon-host/src/http.rs \
  crates/ploy-daemon-host/src/runtime.rs \
  crates/ploy-daemon-host/Cargo.toml \
  crates/ploy-daemon-host/tests/research_agent_queue_postgres.rs \
  migrations/051_research_agent_queue.sql \
  .github/workflows/test.yml
git commit -m "feat(sidecar): execute one durable research action"
```

---

### Task 4: Add truthful PM5D and PM15D strategy-run contract profiles

Files:

- Modify `ploy-frontend/src/lib/agenticStrategyBuilder.ts`.
- Add `ploy-frontend/scripts/check-agentic-builder-contract.mjs`.
- Modify `ploy-frontend/package.json` to include the check in `contracts:check`.

Do not restore the deleted `ploy-frontend/src/pages/StrategyBuilder.tsx` or its `/builder` route. The current operator frontend intentionally exposes only canonical API-backed surfaces; this task hardens the retained packet/contract generator for API or later reviewed UI use.

Profile mapping:

```ts
export type StrategyFamily =
  | "pm5d"
  | "pm15d"
  | "sports"
  | "grok-builder"
  | "market-making"
  | "copy-trading";

export const strategyProfiles = {
  pm5d: "polymarket.pm5d.research_orchestrator.agent",
  pm15d: "polymarket.pm15d.research_orchestrator.agent",
  // existing non-Polymarket profiles remain separate
};
```

Research-only PM5D run contract; the PM15D helper emits the same fields with `market_window_secs = 900`:

```toml
[agentic_strategy_run]
research_only = true
market_window_secs = 300
prediction_horizon_secs = 60
entry_offset_secs = 60
target_label = "settlement_up"
accounting_lane = "settlement_probability"
settlement_source = "official_polymarket"
allowed_symbols = ["BTCUSDT", "ETHUSDT"]
promotion_ready = false

[agentic_strategy_run.tools]
evidence_is_supplied_by_parent = true
dispatch_research_trace_plan = true
dispatch_research_manager_plan = true
dispatch_postgres_portable_export = true
dispatch_event_root_producer = true
dispatch_event_ml_rolling_evidence = true
dispatch_candidate_replay = true
dispatch_recorded_parity = true
prepare_reviewable_dry_run_config_pr = true
record_research_decision_or_typed_prior = true
create_research_issue = true
model_mcp_access = false
model_web_search = false
model_shell_read = false
model_shell_mutation = false
model_controlled_config_write = false
submit_paper_intent = false
submit_live_intent = false
cancel_order = false
redeem = false
apply_deployment = false
set_deployment_state = false
dispatch_deployment = false
```

Contract truthfulness rules:

- Remove `wired` claims for `replay_deployment`, `run_backtest`, `compare_configs`, and `check_oversight` unless an actual callable MCP implementation exists in the repository at implementation time.
- For Polymarket profiles, show the hard-coded PostgreSQL portable export/producer/Event ML/candidate replay/recorded parity/config-PR/evidence-record actions and parent-supplied typed evidence instead. Do not advertise model MCP, shell, or browser access.
- PM5D packet says 300 seconds; PM15D says 900 seconds.
- Both packets carry the full settlement horizon contract; the default entry/prediction offset is an explicit 60 seconds and action symbols must exactly equal `allowed_symbols`.
- Do not expose PM1H as a family; display a concise unsupported/gated note near the horizon selection.
- Remove direct paper/live mutation from Polymarket action steps.
- State that only exact ready handoff plus candidate evidence may let the parent dispatch the dedicated workflow to prepare a reviewable dry-run config PR; training and the child model never write config, deploy, or act live.

Static contract checker asserts:

- both profile strings and windows exist;
- PM1H profile does not exist;
- research-only tool table sets every forbidden capability false;
- no capability map claims an absent research MCP tool is wired;
- model MCP/search/shell/write fields are false and every deterministic research action is present;
- every action packet includes the same full horizon and PM15D cannot emit a 300-second action;
- direct/model-controlled config writes are false, while the separately gated parent config-PR action is present;
- the packet states fixture/diagnostic evidence is not promotion evidence.

Step 1: Add the failing checker and wire it.

```bash
node ploy-frontend/scripts/check-agentic-builder-contract.mjs
```

Expected RED result: PM15D/research-only profile fields are absent and fake MCP capabilities are advertised.

Step 2: Implement profile/packet/contract changes.

- Keep sports/NBA profile behavior intact.
- Reuse one helper for Polymarket market-window mapping rather than duplicating packet text.

Step 3: Verify.

```bash
npm run contracts:check --prefix ploy-frontend
npm run lint --prefix ploy-frontend
npm run build --prefix ploy-frontend
npm audit --omit=dev --audit-level=moderate --prefix ploy-frontend
rtk git diff --check
```

Step 4: Commit.

```bash
git add ploy-frontend/src/lib/agenticStrategyBuilder.ts \
  ploy-frontend/scripts/check-agentic-builder-contract.mjs \
  ploy-frontend/package.json
git commit -m "feat(frontend): add PM5D and PM15D research profiles"
```

---

### Task 5: Document Agent authority and run full local acceptance

Files:

- Add `docs/runbooks/polymarket-research-orchestrator.md`.
- Modify `docs/runbooks/strategy-research-cicd.md`.
- Modify `tasks/research_evidence/TEMPLATE.md` only if the horizon plan has not already added Agent/run URL fields.
- Modify `tasks/todo.md` with exact verification results.

Runbook must include:

- allowed/read-only surfaces;
- typed action table and environment gates;
- isolated empty Codex home, canonical typed evidence embedded directly in stdin, disabled shell/exec/subagent/plugin/browser/MCP tools, strict output-item parser, and dedicated model credential boundary;
- deterministic parent evidence download, SHA/head/horizon validation, schema sanitization, and bounded typed snapshots;
- production PostgreSQL request leases, least-privilege queue role, daemon-sanitized paper context, and file-mode local-only boundary;
- deterministic action ID journal, exact workflow run-name recovery, and ambiguous-outcome no-redispatch rule;
- canonical PostgreSQL export -> portable producer -> Event ML -> candidate replay -> dry-run -> recorded parity action order;
- workflow run URL/artifact/Research OS trace as evidence;
- immutable validated research-decision/typed-prior artifacts as non-promotion Agent state;
- JSONL as transport only;
- forbidden capability list;
- PM5D/PM15D separation and PM1H block;
- dedicated ready-handoff/candidate-gated dry-run config PR workflow;
- explicit statement that no Agent output authorizes live trading.

Full verification:

```bash
npm run contracts:check --prefix ploy-sidecar
npm run test --prefix ploy-sidecar
npm run build --prefix ploy-sidecar
npm audit --omit=dev --audit-level=moderate --prefix ploy-sidecar

npm run contracts:check --prefix ploy-frontend
npm run lint --prefix ploy-frontend
npm run build --prefix ploy-frontend
npm audit --omit=dev --audit-level=moderate --prefix ploy-frontend

if rg -n 'submit_paper_intent = true|submit_live_intent = true|redeem = true|apply_deployment = true' \
  ploy-frontend/src/lib/agenticStrategyBuilder.ts; then
  echo "forbidden Polymarket Agent capability is enabled" >&2
  exit 1
fi
rtk git diff --check
```

Expected result: all build/tests/audits pass; the final `rg` returns no matches; fake command runners prove action behavior; no GitHub issue/workflow/PR or live mutation is created.

Commit:

```bash
git add docs/runbooks/polymarket-research-orchestrator.md \
  docs/runbooks/strategy-research-cicd.md \
  tasks/research_evidence/TEMPLATE.md \
  tasks/todo.md
git commit -m "docs(agent): define Polymarket research authority"
```

## Completion Criteria

- PM5D and PM15D have explicit research-only profiles and horizon values.
- PM1H remains unavailable.
- Model children do not receive wallet, cloud, GitHub, or daemon admin secrets.
- Model children have no shell, filesystem, subagent, plugin, browser, search, MCP, or generic tool execution surface; all typed evidence is supplied in bounded stdin.
- One typed action maps only to hard-coded research workflows/issues/evidence recording.
- A later run can recover a dispatched action only through a validated typed artifact snapshot; metadata-only references are rejected.
- Default external action gates are disabled.
- The deployed research Sidecar receives requests through the PostgreSQL lease queue without a shared trade-host filesystem or daemon credential.
- Any order/cancel/redeem/deployment/self-modification receipt blocks the run even when the tool call failed.
- No false `wired` capability claims remain.
- Queue JSONL never becomes promotion truth.
- A dry-run config PR can only emerge from exact ready handoff plus executable candidate evidence through the dedicated review-only workflow; it cannot merge or deploy.
- Local acceptance performs no external mutation.
