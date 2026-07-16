# Event ML AutoML Workflow

> **Historical standalone PLOY operations.** Host, systemd, secret, and nested
> workflow commands below are retained as migration evidence only. They do not
> define a current Monday deployment or execution path.

This is the canonical research workflow for PM 5-minute event datasets before
moving a signal toward DL, RL, backtest optimization, or live dry-run.

The main rule is simple: optimize the research process before optimizing model
parameters. AutoML-style factor attribution comes before hyperparameter search.

## Scope

Use this workflow when the input is a retained event-root dataset artifact with
a `DatasetBuildManifest` and Parquet observation splits:

- `event_manifest.json`
- `observations_train.parquet`
- `observations_val.parquet`
- `observations_test.parquet`

The workflow assumes binary option accounting:

- one 5-minute event is one decision unit
- train / validation / test events must not overlap
- each model trial uses one or a small fixed set of predeclared entry rows per
  event
- settlement, post-event rows, and future quote state are never allowed as
  features

## Foundation Architecture Artifact

Before adding a new model family, DL experiment, RL environment, or dry-run
handoff, generate the foundation artifact:

```bash
rtk cargo run -p ploy-research --example event_ml_architecture -- \
  --output-dir /tmp/ploy-event-ml-architecture
```

This writes:

- `event_ml_architecture.json`
- `event_ml_architecture.md`
- `event_ml_gate_matrix.json`

The artifact is the shared contract for agents and reviewers. It defines:

- canonical workflow phases
- supervised, DL, RL, and dry-run learning lanes
- required artifacts between phases
- readiness gates for each lane
- stop rules that prevent premature DL/RL work

The architecture layer is intentionally pure Rust and has no Polars, database,
or training-framework requirement. It is the foundation contract; the workflow
runner below is the executable data path.

## Phase 0 - Dataset Contract

Goal: prove the dataset can be trusted as an ML input.

Inputs:

- event-root dataset directory
- manifest generated with the dataset
- observation Parquet files for train, validation, and test

Required checks:

- manifest contract validates
- split event IDs are disjoint
- labels are binary and settled
- entry prices are valid `0 < ask < 1`
- selected rows are before settlement
- selected row count is reported by split

Stop gate:

- Do not train if train / val / test events overlap.
- Do not train if feature normalization would need validation or test data.
- Do not treat tiny selected-event counts as model-quality evidence.

## Workflow Runner

Use this runner when you want the agent or CI-style local execution to follow
the first executable phases in order:

```bash
rtk cargo run -p ploy-research --example event_ml_workflow \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --entry-secs 60 \
  --tolerance-secs 30
```

The runner executes:

1. `event_dataset_coverage`
2. `event_factor_attribution`
3. `event_dataset_baseline`
4. bounded logistic hyperparameter search using `event_dataset_baseline`
5. `event_ml_walk_forward`

It stops on the first failed phase. The attribution phase writes
`factor_attributions.json`, `event_ml_factor_registry.json`,
`event_ml_factor_registry.md`, `feature_whitelist.txt`, and
`feature_whitelist.md`; if the user did not pass `--features`, the baseline
phase consumes that whitelist automatically. The runner also writes
`workflow_report.json` and `workflow_report.md` into the run directory.
The hyperparameter phase writes candidate-level `baseline_metrics.json` files
plus `hyperparameter_search.json` and `hyperparameter_search.md`. Each
`baseline_metrics.json` includes the complete logistic model contract
(`model.kind`, feature schema, intercept, weights, and train-only
standardizer) so a future runtime scorer can replay the same model instead of
scraping report text. The pure scorer contract lives in
`ploy_strategy_bundles::strategies::event_ml_model`; promotion work should
reuse that parser/scorer before wiring any Event ML config PR.
Runtime configs that set
`three_layer_autofactor_runtime_score = "event_ml_model:<name>"` must also set
`three_layer_event_ml_model_path` to the corresponding `baseline_metrics.json`
artifact. The three-layer runtime parses that artifact with the shared Event ML
scorer, validates it before strategy construction, and rejects unsupported
runtime feature schemas fail-closed instead of silently substituting missing
features.
The walk-forward phase writes `walk_forward_report.json` and
`walk_forward_report.md`. It also writes a fail-closed
`event_ml_strategy_handoff.json` / `.md`. The handoff stays `blocked` unless
walk-forward gates pass, total test PnL is positive, a majority of test windows
are positive, a runtime score is explicitly supplied, and replay parity is
marked ready. With only one workflow run, the report is expected to mark DL/RL
and dry-run handoff readiness as `blocked`; that is a useful gate, not a runner
failure.

When replay/runtime parity evidence exists, pass it explicitly through the
workflow runner so the same walk-forward phase can produce a ready handoff:

```bash
rtk cargo run -p ploy-research --example event_ml_workflow \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --runtime-score event_ml_model:baseline_v1 \
  --replay-parity-ready
```

## AutoFactor Strategy Promotion Gate

AutoFactor candidate rows are discovery evidence, not strategy handoff evidence.
After a `factor_walk_forward_v2` report is produced, run the strategy-promotion
evaluator before creating any dry-run handoff:

```bash
python3 scripts/evaluate_autofactor_strategy_promotion.py \
  --report /tmp/factor-walk-forward-v2/report.txt \
  --candidate-strategy-replay-json /tmp/candidate-strategy-replay.json \
  --output-json /tmp/autofactor-strategy-promotion.json \
  --output-md /tmp/autofactor-strategy-promotion.md \
  --output-registry-json /tmp/autofactor-factor-registry.json \
  --output-handoff-json /tmp/autofactor-strategy-handoff.json \
  --output-handoff-md /tmp/autofactor-strategy-handoff.md \
  --factor-registry-preview-json /tmp/factor-registry-preview.json \
  --require-runtime-contract
```

The evaluator requires all of the following before a dry-run handoff:

- the surrounding PRD promotion gate reports `ready_for_dry_run_handoff=true`;
- the AutoFactor row has `decision=candidate` and `reason=passed`;
- the target is an allowed executable target, defaulting to
  `full_depth_settlement_executable_pnl`;
- the factor has an explicit runtime strategy-profile mapping; and
- when a factor-registry preview exists, the factor has a typed, unblocked
  runtime contract with canonical runtime inputs; and
- the runtime profile matches the requested promotion lane, defaulting to
  `settlement_probability`; and
- a candidate strategy executable replay artifact is ready for the exact same
  runtime score, with event-level decisions, full-depth executable accounting,
  official settlement, enough trades, positive ROI/PnL, and no blocking risk
  flags.

Recorded replay/dry-run parity is not a pre-dry-run blocker because it needs
dry-run runtime orders/fills to compare against. It remains a required later
gate before live promotion or risk scaling.

This deliberately blocks cases where a factor is statistically good in the
wrong lane. For example, `spread_adjusted_external_move` can be a valid
repricing candidate while still being blocked from settlement-probability
promotion because its runtime mapping is `repricing_momentum`, not
`settlement_probability`.

The evaluator also emits two machine-readable downstream artifacts when the
corresponding output paths are provided:

- `autofactor-factor-registry.json`: every evaluated factor/target row, its
  AutoFactor status, blockers, runtime mapping, and metrics.
- `autofactor-strategy-handoff.json`: only qualified strategy rows. When no
  row qualifies, this manifest is intentionally `status=blocked` and
  `recommended_action=do_not_promote`.
- `autofactor-strategy-handoff.md`: a dry-run handoff issue/config draft only
  when the handoff is ready. In blocked runs it explicitly says no dry-run
  handoff issue or config should be created.

Factor Walk-Forward V2 also includes a constrained settlement-native generator
for the `full_depth_settlement_executable_pnl` target. It automatically expands
external model-probability full-depth and conservative settlement edge
primitives into `auto_settlement_model_*` formula candidates with near-strike,
capacity, spread, external-pressure, and short-IV-change interactions. PM
quote-implied fair-edge primitives remain diagnostics because they mostly
measure market residuals, not predictive settlement probability. These rows are
still discovery evidence only: they must pass the same promotion evaluator and
runtime mapping gates before becoming a dry-run handoff.
The built-in promotion evaluator maps the generated `auto_settlement_*`
formula family to the `settlement_probability` strategy profile with
`autofactor_formula:<factor_name>` runtime score identifiers. That mapping only
removes the profile/mapping blocker; it does not override the PRD promotion
gate. It also does not replace the strategy replay gate: the same runtime score
must pass historical executable replay before any dry-run handoff issue/config
is created. Recorded replay parity remains the later dry-run/runtime parity gate
after dry-run evidence exists.

The hosted factor walk-forward sweep can build a per-variant
`candidate-strategy-replay.json` from the selected runtime-mappable top bucket
when no external artifact is supplied. That generated artifact has
`basis=factor_walk_forward_top_bucket_aggregate`; it is a diagnostic summary of
top-bucket event accounting, not proof that the deployed runtime scorer will
emit the same decisions on an ordered MarketUpdate stream. The promotion
evaluator must keep such aggregate artifacts blocked until a true runtime replay
artifact for the exact same runtime score is supplied. The sweep still copies
the aggregate JSON and markdown to the artifact root so the next runtime-replay
job can use it as candidate context.

A true pre-dry-run runtime replay artifact must be built from the JSON emitted
by `ploy-runner run --output-json` after replaying the exact candidate config on
an ordered MarketUpdate stream:

For AutoFactor candidates that are not yet deployed, use
`runtime-candidate-replay.yml` from `main`. The workflow treats the deployed
Tango config as a template, writes a temporary replay config, and overrides
`three_layer_autofactor_runtime_score` with the supplied `runtime_score` before
replaying. It does not edit the deployed config or restart a service.

```bash
python3 scripts/build_runtime_candidate_strategy_replay.py \
  --runtime-evaluation-json /tmp/runtime-eval.json \
  --runtime-score autofactor_formula:<candidate> \
  --full-depth-entry \
  --output-json /tmp/candidate-strategy-replay.json \
  --output-md /tmp/candidate-strategy-replay.md
```

This artifact declares `basis=runtime_market_update_replay`. It stays blocked
when the runtime produced zero orders/fills, lacks official settlement rows,
lacks one-event-one-decision evidence, lacks confirmed full-depth entry
accounting, or fails trade-count/fill-rate/PnL thresholds.

For an existing Factor Walk-Forward V2 artifact, use the hosted GitHub workflow
instead of waiting for the self-hosted research runner:

```bash
gh workflow run autofactor-strategy-promotion.yml \
  -f git_ref=main \
  -f factor_walk_forward_run_id=<run-id> \
  -f required_strategy_profile=settlement_probability \
  -f allowed_target=full_depth_settlement_executable_pnl
```

This downloads `factor-walk-forward-v2-<run-id>`, runs the same evaluator on
`report.txt`, automatically uses `candidate-strategy-replay.json` or
`autofactor-candidate-strategy-replay.json` when the source artifact contains
one, uploads `autofactor-strategy-promotion-<run-id>` artifacts, and can
optionally comment on a research issue. If no candidate strategy replay artifact
is present, the handoff stays blocked by design.

The hosted workflow also has a fail-closed `create_handoff_issue` input. Leave
it `false` for diagnostics. When set to `true`, the workflow creates a dry-run
handoff issue only if `autofactor-strategy-handoff.json` reports
`status=ready`; blocked handoffs are logged and skipped.

For the final config-promotion step, the same hosted workflow can create a
reviewable PR instead of requiring a manual TOML edit:

```bash
gh workflow run autofactor-strategy-promotion.yml \
  -f git_ref=main \
  -f factor_walk_forward_run_id=<run-id> \
  -f required_strategy_profile=settlement_probability \
  -f allowed_target=full_depth_settlement_executable_pnl \
  -f create_config_pr=true \
  -f strategy_config=config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml
```

`create_config_pr=true` is intentionally PR-only. It reads the ready handoff
artifact, updates only `three_layer_autofactor_runtime_score` in the target
dry-run config, and opens a normal CI-gated pull request. It does not deploy,
does not change execution/risk settings, and does not enable live trading.
If the handoff is blocked or the config already uses the selected score, no PR
is created.

When the missing piece is a fresh Factor Walk-Forward V2 report and a complete
sampled research snapshot artifact already exists, use the GitHub-hosted
artifact-only workflow:

```bash
gh workflow run factor-walk-forward-v2-hosted-artifact.yml \
  -f git_ref=main \
  -f snapshot_run_id=<snapshot-run-id> \
  -f start_date=2026-04-21 \
  -f end_date=2026-04-25 \
  -f symbols=BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,BNBUSDT,XRPUSDT \
  -f stake_usd=15
```

This workflow runs on `ubuntu-latest`, never reads `PLOY_DB_URL`, and never
compiles a new snapshot. It downloads `research-snapshot-<snapshot-run-id>` by
default, with downstream embedded snapshot fallbacks from
`factor-walk-forward-v2-<snapshot-run-id>` or
`factor-review-v2-<snapshot-run-id>`. It fails before the Rust build if the
artifact only has provenance files and is missing the complete sampled payload
required by `load_research_snapshot`: observations, Deribit rows, and
Polymarket full-book rows.

Dispatch `factor-walk-forward-v2-hosted-artifact.yml` directly with
`snapshot_run_id`. The former `factor-walk-forward-v2.yml` and
`factor-review-v2.yml` router workflows have been removed, so requests without
a retained snapshot artifact no longer have a workflow entrypoint. The old
direct-DB `factor_research` exporter is no longer built or deployed; Event ML
evidence starts from retained event-root dataset artifacts, not from rebuilding
datasets inside an active deploy bundle.

Advanced promotion and issue-handoff controls live in `options_json` to stay
within the GitHub Actions 10-input limit. Defaults are
`required_strategy_profile=settlement_probability`,
`allowed_target=full_depth_settlement_executable_pnl`,
`create_handoff_issue=false`, `create_config_pr=false`, and
`fail_if_blocked=false`. The GitHub-hosted artifact workflow defaults
`report_suite=core`, which keeps the walk-forward report, full-depth execution
matrices, settlement-probability reports, PRD promotion gate, and AutoFactor
mining needed for handoff while skipping slower diagnostic-only sections. Pass
`"report_suite":"full"` when reviewing fillability, liquidity-gated alpha,
trade formation, meta-label, stability, or combo diagnostics.

To make the hosted walk-forward workflow run the final PR-only config promotion
step in the same run, pass `create_config_pr=true` inside `options_json`:

```bash
gh workflow run factor-walk-forward-v2-hosted-artifact.yml \
  -f git_ref=main \
  -f snapshot_run_id=<snapshot-run-id> \
  -f start_date=2026-04-21 \
  -f end_date=2026-04-25 \
  -f symbols=BTCUSDT,ETHUSDT \
  -f stake_usd=15 \
  -f options_json='{"create_config_pr":true,"strategy_config":"config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml"}'
```

This keeps the full chain on GitHub-hosted runners after snapshot creation:
snapshot artifact -> hosted walk-forward -> promotion evaluator -> ready
handoff -> CI-gated config PR. The promotion evaluator still requires a
candidate strategy replay artifact inside the source artifact before the
handoff can be ready. The generated PR updates only
`three_layer_autofactor_runtime_score`; deployment remains a separate explicit
dry-run step from `main`.

For the full settlement-probability PRD gate, prefer passing an existing
complete sampled research snapshot artifact into the orchestrator so downstream
AutoFactor mining and promotion run on GitHub-hosted runners:

```bash
gh workflow run settlement-probability-prd-gate.yml \
  -f git_ref=main \
  -f snapshot_run_id=<sampled-snapshot-run-id> \
  -f start_date=2026-05-01 \
  -f end_date=2026-05-05 \
  -f symbols=BTCUSDT,ETHUSDT \
  -f stake_usd=15 \
  -f audit_lookback_hours=168:event-complete \
  -f replay_parity_run_id=<optional-replay-parity-run-id>
```

The gate requires `snapshot_run_id` by default and dispatches
`factor-walk-forward-v2-hosted-artifact.yml` on `ubuntu-latest`. If
`snapshot_run_id` is omitted, the orchestrator fails before dispatch instead of
falling back to the legacy DB-adjacent `research-snapshot.yml` path. Direct CLI
callers must build or select a retained `research-snapshot.yml` artifact first;
the PRD gate no longer has a legacy snapshot-build exception.
When replay parity evidence uses a non-default artifact name, encode the input
as `replay_parity_run_id=<run-id>:<artifact-name>` so the workflow stays within
GitHub's 10-input dispatch limit.
When runtime candidate replay evidence should feed the next hosted search,
pass it separately as
`options_json.candidate_strategy_replay_run_id=<run-id>` or
`<run-id>:<artifact-name>`. Do not overload `alpha_search_plan_run_id` for this:
alpha search plan artifacts are expected to contain `mcts-expansion-plan.json`,
while runtime replay artifacts are expected to contain
`candidate-strategy-replay.json`.

Use `--output-dir <dir>` to choose the artifact directory. Without it, the
runner writes under `<dataset>/workflow_runs/event_ml_<timestamp>`.

Use `--dry-run` to print the commands without running them, or
`--phases coverage,attribution,baseline,hyperparameter,walk-forward` to run a
subset in canonical order.

When you already have completed workflow runs from prior rolling event-root
datasets, pass them into the current run so the final gate evaluates multiple
windows:

```bash
rtk cargo run -p ploy-research --example event_ml_workflow \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-window-3 \
  --output-dir /tmp/ploy-event-ml-window-3 \
  --walk-forward-run-dir /tmp/ploy-event-ml-window-1 \
  --walk-forward-run-dir /tmp/ploy-event-ml-window-2
```

The current run is always included as one window. The extra run directories
must point at distinct completed workflow runs; repeated run dirs or repeated
dataset windows are treated as blocked evidence, not rolling validation.

For a full multi-window run from multiple event-root datasets, prefer the
rolling orchestrator:

```bash
rtk cargo run -p ploy-research --example event_ml_rolling_workflow \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-window-1 \
  --dataset /tmp/ploy-event-root-window-2 \
  --dataset /tmp/ploy-event-root-window-3 \
  --output-root /tmp/ploy-event-ml-rolling
```

The orchestrator creates `window_001_event_ml`, `window_002_event_ml`, and so
on under `--output-root`. Each window runs the canonical `event_ml_workflow`;
later windows automatically receive earlier completed run dirs as
`--walk-forward-run-dir` inputs. Duplicate dataset paths are rejected before any
work starts.

When you have one larger event-root dataset and need to create distinct rolling
dataset windows first, use the dataset splitter:

```bash
rtk cargo run -p ploy-research --example event_dataset_rolling_windows \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-large \
  --window-events 150 \
  --output-root /tmp/ploy-event-root-rolling
```

The splitter reads the source `event_index.parquet`, observation split Parquet,
and event summary split Parquet, then writes child event-root datasets such as
`event_root_window_001`, `event_root_window_002`, and so on. Each child dataset
gets fresh chronological train / validation / test assignments inside that
window, updated manifest stats, and the standard event-root artifact set. It
also writes `rolling_datasets.txt`, which can be passed to the rolling workflow
runner:

```bash
rtk cargo run -p ploy-research --example event_ml_rolling_workflow \
  --features polars-export -- \
  --datasets "$(paste -sd, /tmp/ploy-event-root-rolling/rolling_datasets.txt)" \
  --output-root /tmp/ploy-event-ml-rolling
```

The default canonical split policy needs at least 134 events per child window
so validation and test each retain at least 20 events. Final remainders smaller
than that are skipped and recorded in `rolling_datasets_report.json`.

## GitHub Rolling Evidence Workflow

For real remote data, use the manual GitHub workflow only after the source
event-root dataset has already been artifactized:

```bash
gh workflow run event-ml-rolling-evidence.yml \
  -f git_ref=main \
  -f source_dataset_run_id=<run-id-with-event-ml-rolling-datasets-artifact> \
  -f start_date=2026-04-24 \
  -f end_date=2026-04-25 \
  -f symbols=BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,BNBUSDT,XRPUSDT \
  -f child_window_events=150 \
  -f run_workflow=true
```

The workflow is artifact-backed only. It builds the required Rust examples on a
GitHub-hosted Ubuntu runner, downloads the existing
`event-ml-rolling-datasets-<run-id>` artifact, and runs the split plus
canonical rolling ML workflow entirely on `ubuntu-latest`. If
`source_dataset_run_id` is empty, the workflow fails closed; the former
`ploy-ci-1` direct-DB export branch is no longer part of the active research
chain.

Prefer the hosted artifact path for every Event ML rolling evidence run.

Use a retained dataset artifact directly:

```bash
gh workflow run event-ml-rolling-evidence.yml \
  -f git_ref=main \
  -f source_dataset_run_id=<run-id-with-event-ml-rolling-datasets-artifact> \
  -f start_date=2026-04-24 \
  -f end_date=2026-04-25 \
  -f symbols=BTCUSDT,ETHUSDT,SOLUSDT,DOGEUSDT,BNBUSDT,XRPUSDT \
  -f child_window_events=150 \
  -f run_workflow=true
```

If the dataset artifact has a non-default name, pass it through
`options_json.source_dataset_artifact_name`. Keep it inside `options_json` so
the workflow stays within GitHub's 10-input dispatch limit.

To let the hosted rolling workflow produce a ready Event ML strategy handoff
after replay/runtime parity has passed, include the handoff gates in
`options_json`:

```bash
gh workflow run event-ml-rolling-evidence.yml \
  -f git_ref=main \
  -f source_dataset_run_id=<run-id-with-event-ml-rolling-datasets-artifact> \
  -f options_json='{"runtime_score":"event_ml_model:baseline_v1","replay_parity_ready":"true","create_handoff_issue":"true"}'
```

Without those options, `event_ml_strategy_handoff.json` remains a blocked
evidence artifact by design. The `create_handoff_issue` option is also
fail-closed: it creates an issue only when the generated handoff JSON reports
`status=ready`.

For the final PR-only dry-run config handoff, keep the same hosted artifact
path and add `create_config_pr=true` plus the reviewed runtime model path:

```bash
gh workflow run event-ml-rolling-evidence.yml \
  -f git_ref=main \
  -f source_dataset_run_id=<run-id-with-event-ml-rolling-datasets-artifact> \
  -f options_json='{"runtime_score":"event_ml_model:baseline_v1","replay_parity_ready":"true","create_config_pr":"true","model_artifact_path":"/opt/ploy/models/event_ml/baseline_metrics.json","strategy_config":"config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml"}'
```

`create_config_pr=true` is supported only on the GitHub-hosted artifact branch.
The generated PR updates only
`three_layer_autofactor_runtime_score` and `three_layer_event_ml_model_path` in
the dry-run strategy config; it does not deploy and does not enable live
trading. The config step fails closed unless the handoff is `ready`, replay
parity is marked ready, the runtime score starts with `event_ml_model:`, and
`model_artifact_path` is supplied.

`event_ml_rolling_workflow` also expects the sibling `event_ml_workflow`
binary to be present next to it in the downloaded artifact, so rolling windows
do not spawn nested Cargo builds on GitHub-hosted runners.

It uploads a compact report artifact by default and deliberately avoids
uploading raw Parquet datasets. Keep `child_window_events=150` for the first
full run because it should keep each child comfortably above the 134-event
split-policy floor.

## Phase 1 - Coverage Diagnostics

Goal: decide whether the dataset is ready for ML, or whether feature coverage
must be fixed first.

Command:

```bash
rtk cargo run -p ploy-research --example event_dataset_coverage \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --entry-secs 60 \
  --tolerances 30
```

Outputs to record:

- train / val / test event counts
- label coverage
- valid price coverage
- finite selected-feature coverage
- per-feature missingness
- selected row coverage at each entry window

Pass criteria:

- enough train events for a fixed baseline
- validation and test have nonzero tradable coverage
- missingness is explained at feature level, not hidden by implicit imputation

Stop gate:

- If row-complete coverage is low, first fix feature export or intentionally
  narrow the feature set. Do not compensate with hyperparameter search.

## Phase 2 - AutoML-Style Factor Attribution

Goal: rank candidate factors and register stable attribution metadata before
model tuning.

Command:

```bash
rtk cargo run -p ploy-research --example event_factor_attribution \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --entry-secs 60 \
  --tolerance-secs 30 \
  --top-n 20
```

What this phase does:

- selects one tradable observation per event near the configured entry time
- computes train / validation / test AUC lift per feature
- computes correlation diagnostics
- ranks by validation AUC lift magnitude
- preserves train-derived direction for registry metadata
- registers factors as `automl:<feature>`

Outputs to record:

- top factors by validation AUC lift
- train / validation / test direction agreement
- stability score
- factor direction
- sample counts
- features rejected for missingness or unstable direction

Pass criteria:

- validation signal is not isolated from train direction
- test direction is not obviously contradictory
- factor has enough coverage at the chosen entry window
- factor is understandable enough to monitor in production

Stop gate:

- AutoML attribution is not a live-edge claim.
- Do not promote a factor only because validation AUC is high.
- Do not use validation or test direction to define registry direction.

## Phase 3 - Feature Set Governance

Goal: freeze a candidate feature schema before model comparison.

Process:

1. Start from the attributed factor list.
2. Keep factors with stable direction and adequate event coverage.
3. Remove near-duplicate factors unless both have a clear reason to stay.
4. Keep negative-direction factors if the direction is stable.
5. Write the feature list into the experiment record.
6. Use train-only normalization parameters.

Artifacts:

- feature whitelist
- rejected-feature list with reasons
- entry-window selection
- normalization metadata

The current executable artifact path is:

- `feature_whitelist.txt`: newline-delimited baseline feature schema
- `feature_whitelist.md`: reviewable feature governance summary
- `factor_attributions.json`: full train/validation/test attribution table
- `event_ml_factor_registry.json`: durable machine-readable registry of
  AutoML-ranked factors, train-derived direction, target label, status, and
  blockers
- `event_ml_factor_registry.md`: reviewable registry summary

Stop gate:

- Do not let each model trial choose a different feature set unless the feature
  selection step itself is explicitly part of the trial and counted as a test.

## Phase 4 - Fixed Baseline

Goal: establish a simple baseline before model selection or tuning.

Command:

```bash
rtk cargo run -p ploy-research --example event_dataset_baseline \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --entry-secs 60 \
  --tolerance-secs 30
```

Default baseline:

- logistic model
- fixed hyperparameters
- train-only normalization
- event-held-out validation and test
- one-event-one-trade simple PnL diagnostic

Outputs to record:

- accuracy
- logloss
- Brier score
- AUC
- simple binary-option PnL
- ROI
- average entry price
- selected event counts

Pass criteria:

- fixed baseline is not broken
- validation and test are not directionally absurd
- PnL accounting uses executable entry prices, not only label accuracy

Stop gate:

- If the fixed baseline fails, do not move to DL, RL, or hyperparameter search.

## Phase 5 - Model Family Selection

Goal: compare model families only after factor governance and fixed baseline are
working.

Recommended order:

1. regularized logistic / linear model
2. small tree or shallow ensemble
3. small neural network
4. sequence model only after multi-row event state is stable
5. RL only after the execution environment and reward accounting are faithful

Rules:

- model families use the same feature schema unless the comparison explicitly
  tracks feature-selection degrees of freedom
- validation chooses the model family
- test is reserved for final confirmation
- every rejected model family counts as a hypothesis test

Stop gate:

- Do not choose the most complex model if it only wins in validation and fails
  basic stability checks.

## Phase 6 - Hyperparameter Search

Goal: tune a selected model family without converting validation into a
backtest-overfit machine.

Use hyperparameter search only after:

- coverage is acceptable
- AutoML attribution produced a governed feature set
- a fixed baseline has passed
- the model family is chosen

Recommended constraints:

- keep parameter count small
- use bounded grids before broad randomized search
- record every search space and rejected trial
- never tune on test
- prefer walk-forward validation once enough days are available

Examples of acceptable search spaces:

- logistic regularization strength
- tree depth / number of leaves
- calibration threshold
- entry threshold
- small ensemble weights

The current executable runner supports a bounded logistic search:

```bash
rtk cargo run -p ploy-research --example event_ml_workflow \
  --features polars-export -- \
  --dataset /tmp/ploy-event-root-5sym-150-20260424 \
  --entry-secs 60 \
  --tolerance-secs 30 \
  --search-l2 0,0.001,0.01 \
  --search-min-edge 0,0.02,0.05 \
  --search-learning-rate 0.03,0.05
```

Selection rule:

- maximize validation PnL
- break ties with lower validation logloss
- record test metrics, but never use test metrics for selection

Artifacts:

- `hyperparameter/candidate_*/baseline_metrics.json`
- `hyperparameter/hyperparameter_search.json`
- `hyperparameter/hyperparameter_search.md`
- embedded baseline `model` contract with feature schema, train-only
  standardizer, intercept, and full logistic weights

Examples to avoid early:

- large neural-network architecture search
- RL reward shaping search
- search spaces with many interacting timing and sizing parameters
- optimizing directly for one tiny test slice

Stop gate:

- If validation improvement does not survive test, rolling windows, or realistic
  PnL accounting, discard the tuned parameter set.

## Phase 7 - Walk-Forward And Backtest

Goal: prove the result is not a single split artifact.

Executable gate command for one or more completed workflow run directories:

```bash
rtk cargo run -p ploy-research --example event_ml_walk_forward -- \
  --run-dir /tmp/ploy-event-ml-run-1 \
  --run-dir /tmp/ploy-event-ml-run-2 \
  --run-dir /tmp/ploy-event-ml-run-3 \
  --output-dir /tmp/ploy-event-ml-walk-forward
```

The gate consumes:

- `workflow_report.json`
- `hyperparameter/hyperparameter_search.json`
- the selected candidate's `baseline_metrics.json`

The gate writes:

- `walk_forward_report.json`
- `walk_forward_report.md`
- `event_ml_strategy_handoff.json`
- `event_ml_strategy_handoff.md`

Default readiness gates:

- at least `3` workflow windows
- at least `3` distinct event-root dataset windows
- each window has at least one test trade
- executable entry accounting is present: cost, ROI, and average entry
- window-level drawdown is reported
- validation/test direction agreement is reported

Dry-run handoff gates:

- walk-forward readiness is `ready`
- total test PnL is positive
- at least half of test windows are positive
- `--runtime-score` is supplied by a runtime-integrated scorer
- `--replay-parity-ready` is supplied only after recorded replay/runtime parity
  evidence has passed

Without those handoff gates, the generated handoff artifact is still useful,
but it must say `status=blocked` and `recommended_action=do_not_promote`.

The workflow runner calls this gate automatically for its current run. A
single-run report should remain `blocked` for DL/RL because it lacks rolling
window evidence.

Required once enough history exists:

- rolling train / validation / test windows
- per-window factor attribution
- per-window selected features
- per-window tuned params, if any
- test PnL distribution by window
- trade count / event count by window

Backtest requirements:

- one-event-one-trade accounting for 5-minute binaries unless a different
  entry policy is explicitly declared
- executable entry quote
- fees / slippage / latency assumptions called out
- payout and average entry price reported
- drawdown and bankroll framing reported

Stop gate:

- Do not advance a strategy on win rate alone.
- Do not call a result production-ready without price, payout, and drawdown
  evidence.

## Phase 8 - DL Gate

DL starts only after the tabular workflow is stable.

Minimum requirements:

- enough events across multiple days
- stable feature export and labels
- sequence/state representation defined before training
- fixed baseline and shallow model baselines recorded
- train-only normalization and no future rows
- OOS test has enough events to matter

DL should answer a specific question, such as:

- does sequence state beat one-row entry features?
- does nonlinear interaction beat the governed factor set?
- does calibration improve executable PnL after costs?

Stop gate:

- Do not use DL because the current dataset is small and noisy.

## Phase 9 - RL Gate

RL is last because it requires an executable environment, not only a label.

Minimum requirements:

- state includes only information available at decision time
- action space is explicit: no trade, buy up, buy down, optional exit
- reward matches binary payout and entry price
- quote availability and latency are modeled
- position sizing and bankroll accounting are modeled
- environment replay produces the same accounting as the backtest

RL should answer a specific question, such as:

- does dynamic entry timing beat fixed entry timing?
- does exit policy improve EV after costs?
- does sizing improve drawdown-adjusted return?

Stop gate:

- Do not start RL while the supervised baseline cannot produce trustworthy
  executable-price accounting.

## Phase 10 - Strategy Handoff

Goal: convert research into a dry-run candidate without changing live behavior
by accident.

Required handoff packet:

- dataset manifest
- feature whitelist
- factor attribution report
- baseline report
- model family decision
- hyperparameter search record, if used
- walk-forward report, if available
- backtest report with executable-price accounting
- rejected alternatives and known risks

Dry-run requirements:

- signal metadata includes model version, feature schema, entry window, and
  factor set ID
- quote availability is monitored
- fill feasibility is monitored
- latency is measured
- no hard live-trade promotion without a separate deployment checklist

## Canonical Order

```text
event-root dataset
  -> coverage diagnostics
  -> AutoML-style factor attribution
  -> governed feature set
  -> fixed baseline
  -> model family selection
  -> hyperparameter search
  -> walk-forward + executable-price backtest
  -> DL gate, if justified
  -> RL gate, if justified
  -> dry-run strategy handoff
```

In short: AutoML finds and governs factors. Hyperparameter search tunes a
chosen model family. DL and RL wait until data, accounting, and execution
semantics are trustworthy.
