# Strategy-Agnostic Research and Runtime CI/CD Runbook

> **Historical standalone PLOY workflow guide.** Workflow references below point to
> nested PLOY files that GitHub does not execute inside Monday. Use the root
> `.github/workflows/ploy-ci.yml` for current PLOY validation.

This runbook defines the generic path for any strategy family to move from an
idea to research evidence, implementation, dry-run observation, parity review,
and promotion or rejection.

PM5D is one current strategy profile, not the center of the CI/CD model. New
families such as sports, copy-trading, market-making, or event-ML should reuse
the same control loop and add only profile-specific configs, data requirements,
and verification thresholds.

## Four-Layer Model

| Layer | Purpose | Output |
| --- | --- | --- |
| Platform CI | Prove code, contracts, dependencies, workflow syntax, frontend, and integration lanes are healthy | Mergeable PR |
| Research CI | Turn a hypothesis into auditable evidence without mutating runtime state | Evidence artifact, issue comment, decision labels |
| Runtime CD | Deploy only reviewed `main` artifacts to protected environments | Remote health and config verification |
| Promotion Gate | Reconcile replay expectations with dry-run behavior before scaling risk | `promote`, `collect-more`, `revise`, `fix-*`, or `reject` |

## Generic Control Loop

```text
Idea / hypothesis
  -> create a research issue
  -> declare strategy_family, profile, data window, assumptions, and success criteria
  -> run the smallest research workflow that can falsify the hypothesis
  -> attach artifact-backed evidence and labels to the issue
  -> if factor/search evidence is positive, run candidate strategy executable replay
  -> if replay proves the same scorer, event-level decisions, full-depth executable prices, fills, and official settlement, create an implementation issue / PR
  -> PR validation through Platform CI
  -> merge to protected main
  -> deploy dry-run from main through protected Runtime CD
  -> verify remote service, config, persistence, and data freshness
  -> compare recorded dry-run behavior with replayed recorded MarketUpdate evidence
  -> decide: promote, collect-more, revise, fix-data, fix-runtime, fix-workflow, or reject
```

The loop is not allowed to enter dry-run after a factor/search result alone.
The replay gate before dry-run is a strategy-level `executable_replay` artifact,
not an IC/ICIR row and not recorded replay parity. It must use the same scorer
that would be configured at runtime, one event-level decision per market event,
full-depth executable entry/depth/fill assumptions, and official settlement.

The loop is not complete after a profitable replay. It is complete only when
the dry-run behavior is reconciled against recorded replay expectations, or
when the mismatch is understood and tracked as a follow-up issue.

## Workflow Map

| Purpose | Workflow | Default role |
| --- | --- | --- |
| PR validation | `.github/workflows/test.yml` | Required Rust-first Platform CI gate for code, contracts, frontend, integration, dependency audit, and workflow lint |
| Legacy Python compatibility | `.github/workflows/legacy-python-tools.yml` | Isolated, path-scoped checks for remaining Python helper scripts; not part of the Rust-first required CI contract |
| Research snapshot | `.github/workflows/research-snapshot.yml` | Compile reusable research evidence from remote data |
| Factor diagnostics | `.github/workflows/factor-review-v2-hosted-artifact.yml` | GitHub-hosted factor review from a retained complete sampled research snapshot artifact |
| Walk-forward diagnostics | `.github/workflows/factor-walk-forward-v2-hosted-artifact.yml` | GitHub-hosted walk-forward from a retained complete sampled research snapshot artifact |
| Parameter optimization | `.github/workflows/optimize.yml` | Bounded train/validation optimization from a retained complete sampled research snapshot artifact |
| Replay/backtest accounting | `.github/workflows/backtest.yml` | Build and run replay/backtest accounting in one job on `ploy-ci-1` |
| Candidate strategy replay | `.github/workflows/runtime-candidate-replay.yml` | Required pre-dry-run proof that the selected runtime score emits executable runtime decisions on a Tango MarketUpdate recording |
| Replay/dry-run parity | `.github/workflows/replay-dryrun-parity.yml` | Compare replay/backtest evidence against a dry-run JSON report |
| Recorded replay/dry-run parity | `.github/workflows/recorded-replay-parity.yml` | Replay a canonical MarketUpdate recording on `tango-1-1` with the deployed binary, then compare against the matching dry-run report slice |
| Runtime evidence reset | `.github/workflows/reset-strategy-runtime-evidence.yml` | Backup-first cleanup for contaminated dry-run/paper `strategy_runtime_orders` and `strategy_runtime_fills`; follow `docs/runbooks/strategy-runtime-evidence-reset.md` |
| Dry-run config sync | `.github/workflows/sync-dryrun-strategy-config-tango-1-1.yml` | Protected config-only sync for a reviewed dry-run strategy TOML; restarts only the target dry-run worker and avoids collector restarts during clean-window waits |
| Event ML rolling evidence | `.github/workflows/event-ml-rolling-evidence.yml` | Produce rolling ML evidence from a retained event-root dataset artifact; `source_dataset_run_id` is required |
| Research Manager plan | `.github/workflows/research-trace-plan.yml` | Read durable Research OS trace on `tango-1-1` with the deployed `research-trace-plan` binary and emit next-step JSON/Markdown artifacts |
| Research Manager executor | `.github/workflows/research-manager-execute-plan.yml` | Convert a trace-plan artifact into dry-run or explicitly ACKed bounded follow-up research dispatches |
| Market data audit | `.github/workflows/market-data-gap-audit.yml` | Scheduled/manual Tango data freshness and gap gate |
| Image build | `.github/workflows/build-push-acr.yml` | Build ACK images; push only immutable checked-out SHA tags |
| ACK deploy | `.github/workflows/deploy-ack.yml` | Deploy immutable SHA image tags through the protected `ack` environment |
| Tango deploy | `.github/workflows/deploy-tango-1-1.yml` | Ship CI-built artifacts to `tango-1-1` and verify host health |
| Trade deploy | `.github/workflows/deploy-trade.yml` | Deploy runner/configs to `ploy-trade-1` through a protected environment |
| Platform bundle | `.github/workflows/release-platform.yml` | Build a portable checksumed artifact only; named host workflows own deployment |
| Live approval | `.github/workflows/approve-live-trade.yml` | Validate exact-SHA replay/dry-run/parity evidence, then require protected human approval before bounded live resume |

`backtest.yml` emits `strategy_backtest_evaluation` as replay/backtest
evidence, not as dry-run or live promotion evidence. Its `data_dir` path now
prefers `orderbook_snapshots` from the full-fidelity CLOB lake under
`/opt/ploy/data/lake/orderbook_snapshots` and falls back to legacy quote ticks
only when no full-depth directory is present. The artifact reports observed
depth-quote coverage and only marks full-depth CLOB fillability when every
non-settlement fill actually used the simulator's `full_depth_sweep`
price basis. Treat profitable metrics as blocked for promotion until the
artifact also has official settlement, event-level lifecycle accounting,
replay/dry-run parity, and runtime scorer parity. The machine-readable fields
`evidence_stage`, `promotion_ready`, `blocking_risk_flags`, and
`advisory_flags` are the operator-facing source of truth for that distinction.

Research data products are intentionally split into four layers:

1. Raw source surfaces: Tango PostgreSQL tables and cold Parquet/ZSTD lake data,
   including full-depth Polymarket CLOB snapshots.
2. Research snapshots: immutable, sampled artifacts for factor search and
   walk-forward. Their `manifest.json` must record sampling cadence, source
   surfaces, row counts, and whether a surface was raw full-fidelity before
   materialization. Each source surface must also use the canonical
   `gate_category` taxonomy: `required_for_prediction`,
   `required_for_execution`, `optional_context`, or
   `missing_blocks_promotion`.
3. Candidate replay tapes: exact `MarketUpdate` sequences and runtime scorer
   inputs used to test one candidate/event lifecycle.
4. Durable research trace: `research_dataset_snapshots`, `factor_registry`,
   `factor_evaluations`, `candidate_replay_tapes`,
   `full_depth_execution_surfaces`, `official_settlement_coverage_checks`, and
   append-only `experiment_trace` rows tying `dsl_hash`, `ast_json`,
   `runtime_contract`, run id, dataset window, execution-surface proof,
   settlement-coverage proof, blockers, and promotion decision together.

Do not call a sampled research snapshot "full data". It can be a complete
retained research artifact for a chosen cadence and window, but full-resolution
execution claims require a candidate replay against the full-depth CLOB lake or
runtime tape.

Use `persist_research_trace` to promote artifact files into the durable
Research OS trace after a snapshot/search run:

```bash
rtk cargo run -p ploy-research --example persist_research_trace --features db -- \
  --run-id <workflow-run-id> \
  --snapshot-dir <research-snapshot-dir> \
  --alpha-search-dir <alpha-search-artifact-dir> \
  --registry-json <optional-promotion-registry.json> \
  --handoff-json <optional-strategy-handoff.json> \
  --full-depth-execution-surface-json <optional-full-depth-execution-surface.json> \
  --db-url "$DATABASE_URL"
```

The writer stores the sampled snapshot manifest, factor registry preview,
factor evaluations, blockers, runtime contracts, and artifact hash chain. It
stores alpha-search rows as `factor_attribution` /
`alpha_search_preview` evaluations keyed by `(dsl_hash, target, horizon)`. It
stores promotion registry, AutoFactor promotion, and handoff artifacts as
`walk_forward` trace rows, not as factor attribution. It does not create dry-run
or live promotion evidence; candidate factors remain `continue` / `candidate`
until executable replay, runtime scorer parity, and dry-run evidence are
attached through the later gates. Candidate replay tapes must use their own
`candidate_replay_id` instead of being collapsed into the sampled research
snapshot identity. Full-depth CLOB execution proof must use a
`full_depth_execution_surface_id` and be queryable through
`full_depth_execution_surfaces`; Research Manager must not infer
execution-surface coverage only by reverse-reading promotion JSON.
Official settlement coverage repair proof must use a valid
`official_settlement_coverage_checks` row for the snapshot window; Research
Manager must not keep treating `pm_token_settlements` as non-materialized when
an executed repair/check proves all candidate market tokens are already
settled and unchanged.

As of current mainline evidence, this chain has been proven through a durable
trace-backed plan, but not through automatic strategy discovery. Hosted
walk-forward run `26336054813` persisted
`evidence_stages=factor_attribution,walk_forward`; Research Trace Plan run
`26336127621` read the trace and produced `theme=fix_data` with
`candidate_count=0`. Treat that as restored plumbing plus a data/candidate
quality blocker, not as a dry-run-ready strategy.

AutoFactor promotion and aggregate candidate-replay builders must prefer the
typed `factor-registry-preview.json` runtime contract when it is available.
When that preview is present, missing runtime contracts, contract blockers,
unsupported inputs, semantic input mismatches, or placeholder runtime fields
block promotion/replay handoff instead of falling back to factor-name
inference.
The cross-language runtime input mapping and formula-blocker rules live in
`config/autofactor_runtime_contract_catalog.json`. Rust alpha-search and Python
promotion/replay tooling must consume that catalog instead of maintaining
separate runtime input allow/block lists.

AutoFactor promotion and candidate replay must also consume the source
snapshot manifest. If a `required_for_execution` surface is marked
`snapshot_sampled=true`, the run can remain factor-search evidence but cannot
suppress global full-depth fillability blockers or claim full-depth executable
handoff from sampled rows. The blocking flag must stay attached to promotion,
handoff, and candidate replay artifacts until replaced by runtime replay or
full-depth lake evidence.

Hosted walk-forward runs the writer by default and requires a protected
database secret unless the dispatch explicitly sets
`options_json.persist_research_trace=false` for a read-only diagnostic run. It
checks `RESEARCH_OS_DATABASE_URL`, `PLOY_DATABASE_URL`,
`PLOY_RESEARCH_DATABASE_URL`, then `PLOY_DB_URL`. When that database URL
targets Tango's private VPC endpoint, the hosted workflow ships the trace input
artifacts to `tango-1-1` and runs the deployed
`/opt/ploy/bin/persist-research-trace` binary there; it must not try to open a
private PostgreSQL connection directly from a GitHub-hosted runner.
Any hosted walk-forward path that mutates follow-up state by dispatching a
chained alpha-search run, opening a dry-run handoff issue, or creating a config
PR must first write the durable trace marker for the same run. Artifact-only
runs may still produce reports, summaries, and issue comments, but they cannot
advance the closed loop.

AutoFactor handoff side effects must also pass
`scripts/validate_autofactor_handoff_replay_gate.py`. A handoff is not allowed
to open a dry-run issue, create a config PR, or satisfy the settlement PRD gate
unless its embedded candidate replay is a ready
`runtime_market_update_replay` from `runtime-candidate-replay.yml` and every
selected strategy runtime score matches that replay tape.

Use `research_trace_plan` after durable rows exist to generate the next
Research Manager plan from DB trace:

```bash
rtk cargo run -p ploy-research --example research_trace_plan --features db -- \
  --db-url "$DATABASE_URL" \
  --evidence-stage factor_attribution \
  --output research-trace-plan.json
```

For CI evidence, dispatch `.github/workflows/research-trace-plan.yml` instead
of running a local DB command. That workflow SSHes to `tango-1-1`, runs the
deployed `/opt/ploy/bin/research-trace-plan` binary against the protected
research database, uploads `research-trace-plan.json` / `.md`, and can attach
the plan to a research issue. It is read-only and does not build Rust or mutate
runtime state.

To convert a Research Manager plan into bounded follow-up actions, dispatch
`.github/workflows/research-manager-execute-plan.yml` with the trace-plan run
id. It defaults to `mode=dry_run`, uploads a machine-readable executor artifact,
and only dispatches follow-up research workflows when `mode=execute` and
`execute_ack=execute-research-manager-plan` are both set. The executor is a
research-only surface: it can dispatch snapshot refreshes or hosted
walk-forward continuations, runtime candidate replay, and recorded replay
parity checks, but it does not deploy, trade, or create config PRs. Runtime
candidate replay must be selected from an explicit workflow input or from a
typed, unblocked runtime contract in the Research Trace Plan
`factor_registry_summary`; when no such contract exists, the executor fails
closed instead of replaying a hardcoded default score.

The current architecture review is
`docs/reviews/research-data-architecture-review-2026-05-25.md`.

Research Manager plans also carry machine-readable `blocker_actions` when
runtime replay or promotion evidence explains why a candidate stayed blocked.
Current actions include official-settlement repair, full-depth execution-surface
collection, broader distinct-event coverage, and high-fillability/depth-filter
mutation. `scripts/research_manager_execute_plan.py` copies those actions into
`research_manager_typed_prior.v1` so the next hosted search can consume the
constraint instead of relying on a human to reinterpret the markdown blocker.
Full-depth execution-surface collection is a separate lane from sampled research
snapshot refresh: `collect_full_depth_execution_surface` dispatches
`.github/workflows/collect-full-depth-execution-surface.yml`, which uses the
deployed CLOB archive exporter on `tango-1-1` and emits
`full_depth_execution_surface.v1`. `repair_official_settlement_coverage`
dispatches `.github/workflows/repair-official-settlement-coverage.yml`, which
defaults to `mode=dry_run` and only mutates `pm_token_settlements` when the
Research Manager executor itself is run with execute mode plus the required ACK.
Do not treat a sampled snapshot rerun as settlement repair.

`recorded-replay-parity.yml` defaults `since=auto` and `until=auto`. In auto
mode it scans the target recording on `tango-1-1`, intersects that recording
coverage with the dry-run report for the target deployment, prefers the latest
closed dry-run rows when available, and falls back to current open rows when a
fresh recording has not yet accumulated closed events. The workflow records the resolved window in the workflow summary, issue comment, and
`resolved-window.json` artifact. Manual timestamps remain supported for
reproducing a known incident window or an older research issue.
The workflow is read-only evidence generation. `runner_source=workflow_ref` is
the default so promotion evidence is bound to the exact reviewed Git SHA: it
builds `new-ploy-runner` on the GitHub runner, copies that temporary binary to
the replay scratch directory on `tango-1-1`, and compares it against the
deployed config/recording. Use `runner_source=deployed` only as a diagnostic for
the currently installed Tango binary; deployed-runner evidence cannot satisfy
the protected live-approval gate. Both modes
use a temporary replay config plus report/database reads. The workflow does not
deploy artifacts, restart services, replace `/opt/ploy/bin/ploy-runner`, or
enable live orders. Its `approval_environment` input therefore defaults to
`tango-1-1-build-only` so parity checks can run without the protected live
deploy approval gate. Use `approval_environment=tango-1-1` only when an operator
explicitly wants the protected environment approval for an incident replay.
The build-only environment must still provide `TANGO_1_1_KNOWN_HOSTS` for
pinned host verification. The SSH key may come from the protected environment
secret or from the repository-level `TANGO_SSH_KEY` / `ALIYUN_ECS_SSH_KEY`
fallbacks used by read-only research workflows.
For the active settlement-probability BTC/ETH dry-run profile, use the matching
recording path
`/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson`;
the older `pm5d-threelayer-canonical.ndjson` recording is historical and can
fail auto-window resolution because it does not overlap current dry-run rows.
If auto-window resolution reports that dry-run rows do not overlap recording
coverage, first check whether the recording has reached its configured
`record_market_updates_max_records` or `record_market_updates_max_bytes` cap.
Use `sync-dryrun-strategy-config-tango-1-1.yml` with `execute=true`,
`force_restart=true`, and `rotate_recording=true` to archive the capped
recording and restart only the target dry-run worker before collecting a fresh
parity window. This keeps the old recording as an archive instead of deleting
raw evidence.

## Research Issue Contract

Every research issue must describe one testable claim. Use child issues when a
strategy direction splits into independent factors, filters, data checks, or
execution assumptions.

Required fields:

- `strategy_family`: broad family such as binary-options, sports, copy-trading,
  market-making, or event-ML.
- `strategy_profile`: concrete profile/config under test, or `new` if it does
  not exist yet.
- `hypothesis`: the single claim this issue should prove or falsify.
- `expected edge mechanism`: why the idea should survive spread, fees, stale
  quotes, queue position, settlement, and execution friction.
- `required data`: tables, symbols, windows, labels, settlement fields, and
  freshness checks.
- `workflow plan`: the workflow and exact inputs to run.
- `success and failure criteria`: thresholds for continue, revise, reject, or
  promote.
- `accounting contract`: whether the workflow is exploratory diagnostics or
  executable strategy accounting.
- `parity plan`: how replay/backtest behavior will be compared against dry-run
  behavior.

Evidence block:

```text
Evidence:
- Workflow:
- Run URL:
- Git ref:
- Strategy family/profile:
- Dataset/window:
- Symbols/markets:
- Config:
- Artifact:
- Headline metrics:
- Replay/dry-run parity:
- Caveats:
- Decision:
```

Backtest evidence must explicitly state whether it is exploratory diagnostics or
executable strategy accounting. Event-scoped strategies must not count multiple
diagnostic rows as multiple deployable trades unless the runtime can execute the
same entries under the same risk rules.

For PM5D AutoFactor handoff, promotion also requires the AutoFactor report's
top deployable bucket to expose event-level decision fields. Missing
`top_bucket_unique_event_count` / `top_bucket_max_event_decisions` blocks
handoff, and `top_bucket_max_event_decisions > 1` blocks handoff because the
candidate is still relying on repeated rows from the same event.
The attached runtime replay tape must also carry the same target/horizon as
the promoted factor row. A 30s repricing replay cannot back a 5m settlement
handoff, even when the runtime score string and aggregate metrics look
compatible.

## Decision Labels

Research workflows write evidence comments and apply labels so the issue queue
can be filtered without reading every artifact.

Evidence labels:

- `evidence:factor-review`
- `evidence:walk-forward`
- `evidence:optimize`
- `evidence:backtest`
- `evidence:parity`
- `evidence:missing-artifact`
- `evidence:missing-metrics`

Decision labels:

- `decision:pending`
- `decision:continue`
- `decision:collect-more`
- `decision:promote`
- `decision:reject`
- `decision:revise`
- `decision:fix-data`
- `decision:fix-runtime`
- `decision:fix-workflow`

Parity labels:

- `parity:blocked`
- `parity:ready`

## Promotion Rules

Do not promote a strategy from research directly to live. Promotion is staged:

1. `decision:continue` or a positive factor/search artifact means run
   candidate strategy executable replay next; it does not mean dry-run.
2. `decision:promote` on the research issue requires a ready candidate
   strategy replay artifact for the exact runtime score, profile, event-level
   decision contract, full-depth executable accounting, and official settlement.
   Only then can it become an implementation issue or PR.
3. Runtime code/config changes must pass Platform CI and merge to `main`.
4. Dry-run deployment must be triggered from `main` through a protected
   environment.
5. Remote verification must prove service health, expected config, persistence,
   data freshness, and no on-host Rust build.
6. Recorded replay/dry-run parity must either report `strict_parity_ready=true`
   or create a follow-up issue explaining the mismatch.
7. Live promotion requires a separate approval with explicit stake, loss, and
   rollback limits.

## Current Strategy Profiles

Current workflows still include profile defaults for the active binary-options
line. Treat these as defaults, not architecture:

| Family | Profile/config examples | Notes |
| --- | --- | --- |
| binary-options | `02-pm5d-threelayer.*.toml`, `pm5d.threelayer.*.dryrun` | Current most exercised strategy family |
| event-ML | event-root rolling evidence workflows | Research/data pipeline, not a live strategy by itself |

When adding a new family, update configs and workflow inputs so the family can
reuse the same evidence, PR, deploy, and parity loop. Do not fork a separate
CI/CD architecture unless the data plane or runtime target is genuinely
different.

## Control-Plane Rules

- Research workflows can run on feature branches when they do not mutate
  deployment state.
- Keep `workflow_dispatch` inputs at or below GitHub's 10-input limit. Put
  advanced or rarely changed knobs into `options_json`, validate keys in the
  workflow, and fail on unknown options.
- Deployment workflows that affect `tango-1-1`, `ploy-trade-1`, ACK, or
  production state must run from `main`.
- Host deployment workflows must be dispatched from `main` with `git_ref=main`
  before mutating remote state.
- Tango and trade SSH deploys require pinned `known_hosts` secrets
  (`TANGO_1_1_KNOWN_HOSTS` and `PLOY_TRADE_1_KNOWN_HOSTS`). Entries should be
  keyed by the workflow aliases `tango-1-1` and `ploy-trade-1` because the deploy
  SSH config sets `HostKeyAlias`.
- Artifact-backed PM5D/PM15D research should default to GitHub-hosted runners.
  Use `factor-review-v2-hosted-artifact.yml`,
  `factor-walk-forward-v2-hosted-artifact.yml`, or
  `settlement-probability-prd-gate.yml` with `snapshot_run_id`, when a retained
  complete sampled research snapshot artifact already exists.
- For settlement-probability searches using the `pm5d-execution` data profile,
  Deribit IV/Greeks are not a required promotion surface. Set
  `options_json.require_deribit=true` only for PRD or volatility hypotheses that
  intentionally require `pm5d-vol` / Deribit evidence.
- For one-day PM5D OOS smoke runs, keep the evidence stage as `walk_forward`
  but use hourly windows such as
  `options_json.train_window_hours=12`, `test_window_hours=12`, and
  `step_hours=12` on a clean 24h snapshot. This is an early OOS filter, not a
  replacement for longer promotion-grade rolling evidence.
- Event ML rolling evidence should also default to GitHub-hosted runners after
  the source event-root dataset is artifactized. Pass `source_dataset_run_id`
  to `event-ml-rolling-evidence.yml`; the workflow no longer exposes its
  former direct-DB `ploy-ci-1` export branch.
- `factor-review-v2.yml` and `factor-walk-forward-v2.yml` have been removed.
  Run `research-snapshot.yml` first, then dispatch the hosted artifact
  workflows directly with `snapshot_run_id`. Factor search no longer has a
  legacy router workflow or direct-DB execution path.
- Remaining DB-mode research workflows must fail closed unless the research
  database URL targets Tango's private VPC endpoint `172.16.0.204`. A public
  Tango endpoint can turn large backtest query results into billable公网出流量.
- ACK/ACR image workflows must use immutable checked-out commit SHA tags only.
  Do not push or deploy `latest`. ACK deployments must also pass through the
  protected `ack` environment before mutating the cluster.
- Runtime deployment evidence must include remote host verification, not only a
  successful workflow conclusion.

## Remaining Improvements

1. Make dry-run reports and replay artifacts expose the same strict event-level
   fields so parity can become a full proof instead of a readiness gate.
2. Move family/profile selection into first-class workflow inputs where the
   implementation currently relies on profile-specific defaults.
3. Add automated metric parsers for factor, walk-forward, and optimize evidence
   so workflows can label `decision:collect-more`, `decision:reject`, or
   `decision:promote` without manual review.
