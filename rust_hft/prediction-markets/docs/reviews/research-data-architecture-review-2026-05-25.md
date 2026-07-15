# Research Data Architecture Review - 2026-05-25

Evidence stage: `diagnostic` / architecture review. This review describes data
and research-chain readiness. It is not dry-run, live, or strategy promotion
evidence.

Reviewed refs:

- `origin/main` at `1a938f7f0f5c68ea8690b0a28af3c8d43af1dfc5`
- cleanup branch `cleanup/research-legacy-docs-and-review`

## Executive Verdict

The research chain is now separated enough to run an automatic loop:

```text
research-snapshot.yml
  -> factor-walk-forward-v2-hosted-artifact.yml
  -> persist Research OS trace
  -> research-trace-plan.yml
  -> research-manager-execute-plan.yml
  -> runtime-candidate-replay.yml / recorded-replay-parity.yml
  -> new hosted walk-forward or rejection
```

That means Ploy can automatically produce factor attribution, walk-forward
diagnostics, durable trace rows, Research Manager plans, bounded follow-up
workflow dispatches, runtime candidate replay, and rejection decisions.

It has not yet proven an automatically tradable strategy. The contaminated
dry-run baseline was reset in run `26369076183`, which reported zero orders,
zero trades, zero closed trades, and zero open positions after reset. The
following candidate-quality gate run `26370065657` then failed on economics and
sample size, not on buy-side fillability: `closed_trades=3`, `realized_pnl=-11.49`,
`profit_factor=0.2372`, `buy_fill_rate_pct=99.59`, and
`max_drawdown=-15.0571`. That is a reject/revise signal, not strategy approval.

## Current Data Shape

| Layer | Current truth surface | Review |
| --- | --- | --- |
| Raw collector data | Tango PostgreSQL tables, including `binance_lob_ticks`, quote ticks, trades, settlements, and hot `clob_orderbook_snapshots` | Raw data is the source material. Research workflows should compile artifacts from it instead of repeatedly joining raw tables during every search. |
| Full-depth CLOB archive | `/opt/ploy/data/lake/orderbook_snapshots/date=YYYY-MM-DD/hour=HH/` with Parquet/ZSTD, manifests, and `_SUCCESS` markers | This is the right execution-depth lake. It is full-fidelity by policy and separate from sampled research snapshots. |
| Research snapshots | Retained `research-snapshot.yml` artifacts with manifest, source surfaces, sampling, quality report, and data audit | Correct canonical input for factor search and walk-forward, but still sampled. A sampled snapshot is not full-depth execution proof. |
| Runtime replay tapes | `runtime-candidate-replay.yml` artifacts and `candidate_replay_tapes` Research OS rows | Correct pre-dry-run proof surface when `basis=runtime_market_update_replay`, official settlement is complete, and full-depth entry is confirmed. |
| Durable Research OS state | PostgreSQL tables `research_dataset_snapshots`, `factor_registry`, `factor_evaluations`, `experiment_trace`, `candidate_replay_tapes`, and `full_depth_execution_surfaces` | Correct durable layer. This should remain the queryable system of record for research lineage, execution-surface coverage, and promotion decisions. |
| DuckDB | Local/CI Parquet query accelerator and export helper | Useful execution engine, not durable state. The problem is not "data is not in DuckDB"; the problem is whether artifacts preserve the right fidelity and are linked into trace. |

## What Is Now Correct

1. **Legacy PRD snapshot fallback is removed.**

   PR `#686` removed `--allow-legacy-snapshot-build` from
   `scripts/run_settlement_probability_prd_gate.py`. The settlement-probability
   PRD gate now only consumes retained research snapshots and fails before
   dispatch when `--snapshot-run-id` is missing.

2. **AutoFactor runtime input projection has a shared catalog.**

   PR `#685` added `config/autofactor_runtime_contract_catalog.json` and made
   Rust alpha-search plus Python promotion/replay helpers consume it. Unsupported
   inputs such as `external_pressure` and `iv_change_1m` are contract blockers,
   not silently inferred runtime fields.

3. **Research lineage has durable queryable state.**

   Migrations `042`, `043`, `044`, and `047` define `factor_registry`,
   `factor_evaluations`, append-only `experiment_trace`,
   `research_dataset_snapshots`, `candidate_replay_tapes`, and
   `full_depth_execution_surfaces`. This is the correct place for `dsl_hash`,
   `ast_json`, runtime contract, run id, dataset window, blockers, replay
   identity, full-depth proof identity, and promotion decision.

4. **Legacy router workflows are removed.**

   `factor-review-v2.yml` and `factor-walk-forward-v2.yml` have been removed.
   Factor review and walk-forward now dispatch directly through the hosted
   artifact workflows, which require `snapshot_run_id`.

5. **Full-depth CLOB is no longer confused with sampled snapshots.**

   `collect-full-depth-execution-surface.yml` produces
   `full_depth_execution_surface.v1` from the CLOB orderbook archive. Hosted
   walk-forward can consume that proof to satisfy execution-surface blockers
   for matching windows.

6. **Manual direct-DB factor research has been retired from active code.**

   The active examples `factor_review_v2` and `factor_walk_forward_v2` are now
   snapshot-only, and the old `scripts/run_factor_research*.sh` direct-DB shell
   runners have been removed. Direct raw-table reads remain source compilation
   work for `research-snapshot.yml`, not a second factor-discovery path.
   The legacy direct-DB `factor_research` exporter is also no longer registered
   as a `ploy-research` example, built by Tango deploy, installed as
   `/opt/ploy/bin/factor-research`, or packaged in the ACR research image.
   Event ML now starts from retained event-root dataset artifacts rather than a
   deployed direct-DB exporter.

7. **The old PM5D matrix diagnostics are no longer active research entrypoints.**

   The former `strategy-research-matrix.yml` and `dryrun-correction-matrix.yml`
   family predated the current Research OS loop. Those workflows, their
   `ploy-research` example binaries, and their artifact-only analyzers have
   been retired so strategy discovery routes through retained snapshots,
   hosted factor/search workflows, runtime replay, durable trace, and Research
   Manager actions instead of a parallel `ploy-ci-1` matrix surface.
   Unreferenced direct-DB diagnostic examples `factor_scan` and
   `collector_data_utilization` have also been removed.

8. **The optimizer workflow is snapshot-only.**

   `.github/workflows/optimize.yml` now requires a retained complete sampled
   research snapshot artifact and builds only the `three_layer_snapshot_optimize`
   runner. The old live-Parquet / `optimize_backtest` workflow branch, deploy
   binary, example source, and verification script/runbook have been retired
   from the active research surface.

## Remaining Architecture Problems

### P0 - Full-depth trace is coded but not deployed on Tango yet

`full_depth_execution_surfaces` is now in `origin/main`, and
`research_trace_plan` can query durable full-depth coverage. The latest Tango
deploy for `main@6e711725` is still waiting on the protected `tango-1-1`
environment, so the remote database/binaries have not yet been refreshed.

Done means deploy run `26370807170` or a newer `main` deploy succeeds, the
remote migration is applied, `/opt/ploy/bin/research-trace-plan` is refreshed,
and a `research-trace-plan.yml` run proves that Research Manager can read
`full_depth_execution_surfaces`.

### P0 - Research Manager full-depth collection must cover the whole evidence window

Research Manager-dispatched full-depth surface repair must produce a proof that
can satisfy `research_trace_plan` coverage checks for the sampled snapshot
window. This cleanup branch fixes the executor default so Research Manager
collects the full dataset window and marks incomplete collection fail-closed;
bounded hour caps should be treated as diagnostic-only, not promotion-grade
repair evidence until the fix is merged and deployed.

### P0 - Official settlement completeness remains a hard blocker

Settlement-probability candidates must not pass promotion unless every traded
event has official settlement coverage. Settlement repair workflows can produce
evidence, but missing settlement rows must route to data repair, not economics
approval.

### P0 - Tick-level LOB and sampled factor snapshots need an explicit contract

The user concern about "LOB should be WebSocket/tick-level, not 30 seconds" is
valid at the raw-data layer: the collector path stores `binance_lob_ticks` and
the tick-preserving optimize runbook explicitly forbids adding a bucket/stride
layer to raw LOB replay. However, factor walk-forward snapshots intentionally
compile sampled observations by default.

The missing contract is not "put everything in DuckDB." The missing contract is
two named products:

- **sampled factor snapshot**: compact factor-attribution tape, with sampling
  cadence recorded and no full-depth execution claim.
- **tick/full-depth execution tape**: ordered MarketUpdate or Parquet tape used
  for candidate replay, fillability, slippage, and runtime parity.

Until those are distinct in every artifact and trace row, agents can still ask
the wrong question: "what is the return of this sampled factor row?" instead of
"can this event-level runtime decision be filled at the configured stake?"

### P1 - Label contracts are only partially centralized

`config/autofactor_accounting_catalog.json` centralizes AutoFactor target,
horizon, lane, strategy profile, and execution requirements. That is good for
AutoFactor, but the broader 30s / 60s / 5m / 15m label engine is not yet a
generated shared contract across research, runtime replay builders, and trace
persistence.

### P1 - Runtime input catalog is shared for AutoFactor, not all feature lanes

`config/autofactor_runtime_contract_catalog.json` fixed the immediate
AutoFactor promotion problem. Event ML, LOB ML, optimize/backtest lanes, and
future non-AutoFactor runtime scorers still need generated feature schemas with
the same fail-closed semantics.

### P1 - Research Manager needs stronger negative-economics actions

The loop can reject and revise, but persistent negative executable ROI should
produce stronger family-level avoidance and explicit new search constraints.
Otherwise the closed loop can keep rediscovering variants of the same
economically weak thesis.

### P2 - Some older architecture docs are now superseded

`docs/architecture/research-backtest-rearchitecture.md` still describes the
April proposed design with `ploy-ci-1` as the research runner. Current active
workflow docs supersede that for factor search: retained snapshots are built
from Tango data and consumed by GitHub-hosted artifact workflows, while durable
trace persistence runs through Tango only when private DB access requires it.

## DuckDB Answer

DuckDB should not become the authoritative durable layer. It is the right tool
for fast Parquet scans, export verification, and bounded replay loading. The
durable layer should stay:

```text
PostgreSQL Research OS tables
  + immutable GitHub artifacts
  + cold Parquet/ZSTD lake files
```

The concrete fix is to ensure every DuckDB/Parquet product has a manifest hash,
window, source-surface contract, sampling/fidelity declaration, and durable
trace pointer. Moving state into DuckDB without those contracts would make the
architecture less auditable.

## Next Work

| Priority | Work | Done when |
| --- | --- | --- |
| P0 | Deploy current `main` to Tango and rerun Research Trace Plan | Protected deploy succeeds, migration `047` is applied, and `research-trace-plan.yml` reads `full_depth_execution_surfaces` |
| P0 | Reject/revise the current dry-run candidate based on clean candidate-quality evidence | Research Manager uses run `26370065657` economics/sample-size failure as a blocker instead of re-promoting the same candidate |
| P0 | Keep official settlement completeness fail-closed | Every replay-fed handoff proves official settlement for all traded events |
| P0 | Split sampled factor snapshots from tick/full-depth execution tapes in manifests and trace | Agents cannot use sampled factor rows as full-depth execution evidence |
| P1 | Generate shared multi-horizon label contracts | Research, replay, runtime, and trace persistence use one target/horizon/accounting schema |
| P1 | Expand shared runtime feature schemas beyond AutoFactor | Event ML, LOB ML, optimize/backtest, and future runtime scorers all fail closed on unsupported inputs |
| P1 | Strengthen negative-ROI Research Manager priors | Repeated negative runtime replay closes or strongly penalizes weak factor families |
| P2 | Mark stale architecture docs superseded | Readers land on this current review or the current runbooks instead of the April proposed design |

## Can We Start Researching Strategies?

Yes, but only in the strict loop:

1. Use retained research snapshots for factor discovery.
2. Require typed runtime contracts before candidate replay.
3. Require runtime MarketUpdate replay, full-depth entry, official settlement,
   and positive executable ROI before dry-run handoff.
4. Treat sampled snapshot metrics, fillability diagnostics, and backtest-only
   PnL as research evidence, not tradable strategy evidence.

The architecture is good enough to continue automated discovery. It is not good
enough to relax promotion gates.
