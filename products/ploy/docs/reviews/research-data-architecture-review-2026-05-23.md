# Research Data Architecture Review - 2026-05-23

Last updated: 2026-05-24 against `main`
`f887d944ed8fd1c3d1232244fd65b839dfc821cc`.

## Scope

This review covers the PM5D / AutoFactor research chain after the Research OS
trace migration and the first legacy workflow cutover:

1. Raw source surfaces.
2. Complete sampled research snapshots.
3. Alpha-search / factor attribution.
4. Candidate replay and runtime parity.
5. Durable Research OS trace and Research Manager planning.

It does not approve dry-run or live promotion. The current reviewed stage is
`factor_attribution` / `walk_forward` infrastructure, with candidate replay
reserved for `executable_replay` evidence.

## Current Shape

| Layer | Current state | Review |
| --- | --- | --- |
| Raw source surfaces | Tango PostgreSQL plus cold lake paths for CLOB/orderbook data | Exists, but not all research runners consume raw/full-resolution surfaces directly |
| Research snapshot | `research_snapshot_compile` emits immutable complete sampled artifacts with source-surface metadata and canonical `gate_category` values | Canonical factor-search input; not full-resolution execution replay evidence |
| Alpha-search | Hosted artifact walk-forward emits registry previews, typed runtime contracts, MCTS tree, search feedback, promotion/handoff artifacts | Usable for factor discovery and attribution |
| Candidate replay | `candidate_replay_tapes` exists and `persist_research_trace` can link replay artifacts to factor evaluations | Durable replay identity exists, and hosted walk-forward can now fan out runtime replay requests from unblocked runtime contracts |
| Durable trace | Hosted walk-forward defaults to trace persistence and writes Research OS rows when DB secrets and deployed binaries are available | Current `main` run `26350924329` persisted trace and uploaded `research-trace/persisted.env` |
| Research Manager | `research_trace_plan` can read durable DB trace and emit next-plan JSON; `research-manager-execute-plan.yml` and hosted walk-forward can dispatch bounded follow-up evidence | Main runs have proven planner -> executor -> hosted walk-forward -> durable trace -> planner recovery with runtime replay and full-depth evidence inherited automatically |
| Legacy research paths | Factor routers and Event ML rolling evidence are artifact-backed only | Former direct `ploy-ci-1` DB execution/export branches have been removed from the active research chain |
| Diagnostic backtest | `backtest.yml` now emits `evidence-stage` artifacts with `promotion_ready=false` | Useful diagnostic replay/backtest surface; not promotion evidence |
| Legacy repo entrypoints | Retired local CSV/discovery/debug helpers, public-profile dry-run prototypes, legacy root-runtime workflows, and archived systemd assets have been removed from the active repository | Active docs now point to artifact-backed research, canonical platform release, and runtime replay surfaces |

## Fixed In This Migration

1. **Backtest evidence can no longer imply promotion.**

   `backtest.yml` publishes an explicit `diagnostic` evidence-stage contract,
   labels issues with `evidence:diagnostic`, and forces
   `promotion_ready=false` even if an underlying artifact claims otherwise.

2. **Factor review is explicitly attribution-only.**

   `factor-review-v2-hosted-artifact.yml` publishes machine-readable
   `factor_attribution` stage artifacts and blocks `dry_run_candidate` /
   `live_candidate` as next stages.

3. **Candidate replay is a durable trace object.**

   Migration `044_candidate_replay_tapes.sql`, the Research OS registry types,
   and `persist_research_trace` now persist candidate replay identity,
   provenance, evidence stage, runtime score, strategy profile, metrics,
   blockers, and links to `factor_evaluations`.

4. **AutoFactor side effects require durable trace provenance.**

   Promotion side effects now require the persisted trace marker plus snapshot
   provenance, reject direct-DB/debug/self-hosted source markers, and validate
   that the ready handoff embeds a runtime candidate replay tape before handoff
   issue or config PR creation.

5. **Legacy factor direct-DB branches are removed.**

   `factor-review-v2.yml` and `factor-walk-forward-v2.yml` no longer build or
   run `ploy-research` on `ploy-ci-1`. They route snapshot-backed requests to
   the GitHub-hosted artifact workflows and fail closed when `snapshot_run_id`
   is missing.

6. **Legacy Event ML direct-DB export is removed.**

   `event-ml-rolling-evidence.yml` no longer builds the database-backed
   `factor_research` exporter or runs on `ploy-ci-1`. It requires an existing
   event-root dataset artifact via `source_dataset_run_id` and fails closed when
   that artifact provenance is missing.

7. **Shared AutoFactor accounting is now generated from one active catalog.**

   `config/autofactor_accounting_catalog.json` is consumed by Rust
   alpha-search/persistence code and Python promotion/replay builders. Runtime
   candidate replay now rejects target/horizon mismatches against the selected
   factor row before emitting replay evidence.

8. **Active sampled snapshot naming has been cut away from legacy `full`
   terminology.**

   Active dispatch/provenance fields now use `upload_sampled_snapshot` and
   `sampled_snapshot_embedded`. `research-snapshot.yml` keeps a backward
   compatible input alias for older dispatch payloads, but downstream artifacts
   no longer describe retained sampled products as unsampled source data.

9. **Closed-loop runtime replay fan-out is automated.**

   Hosted walk-forward now emits a deduped `runtime_replay_requests` batch while
   preserving the legacy single `runtime_replay_request` field. Dispatch is
   gated on successful durable trace persistence and closed-loop
   `action=fix_runtime`. Contract-blocked candidates, including runtime input
   semantic mismatches such as `external_pressure`, are excluded before
   dispatch.

10. **Legacy research and host-support entrypoints are removed from the active
    repository.**

    PRs `#643`, `#644`, and `#645` first isolated duplicate DB diagnostics,
    local CSV collectors, market-discovery prototypes, public-profile dry-run
    scripts, manual root-runtime deploy helpers, and the old
    `install-service.sh` entrypoint. The current cleanup removes those archived
    executable assets instead of preserving a second runnable architecture in
    the repo. `release-platform.yml` and `install-platform-service.sh` own
    platform service plus maintenance/watchdog timer installation.

11. **The active release path preserves host guardrails without the old
    installer.**

    `install-platform-service.sh` now installs `ployd.service`,
    `ploy-maintenance.timer`, and `ploy-platform-watchdog.timer`. The release
    workflow bundles the timer units and support scripts, installs them on the
    host, and verifies timer status after deploy. The retired installer is no
    longer kept as an executable repo asset.

12. **Research snapshot orchestration now has observable fallbacks.**

    PRs `#656`, `#657`, `#658`, and `#659` made the snapshot data audit report
    show the exact gate/window being audited, retried SSH audit/compile/copy
    steps, added a Cloud Assistant fallback for `tango-1-1`, and made terminal
    fallback failures print decoded remote output. This restored the remote
    snapshot path without compiling Rust on the trading host.

13. **Research Manager executor now inherits evidence artifacts from trace.**

    PR `#673` made `research-manager-execute-plan.yml` infer the latest
    `runtime_market_update_replay` artifact and reusable full-depth execution
    proof from the Research Trace Plan input. The follow-up hosted
    walk-forward run no longer needs manual hidden artifact IDs to avoid
    falling back to aggregate replay or sampled execution blockers.

## Current Verified Runs

| Run | Git ref / source | Result | Meaning |
| --- | --- | --- | --- |
| `26336054813` | `main` at `a5234cbfdf3cb93c22aa013888e418b83d308399` | Hosted walk-forward succeeded; `persisted=true`; stages `factor_attribution,walk_forward` | Snapshot -> walk-forward -> durable trace path is working from current main |
| `26336127621` | `main` Research Trace Plan | Succeeded; `schema_version=research_trace_plan.v1`; `theme=fix_data`; `candidate_count=0` | Research Manager can read trace and produce a typed next plan |
| `26340352094` | `main` deploy to `tango-1-1` | Succeeded; SSH path passed and Cloud Assistant fallback skipped | Runtime repair/deploy path is healthy from CI-built artifacts |
| `26340671822` | `main` Research Manager executor | Succeeded; dispatched runtime candidate replay `26340673836` and recorded replay parity `26340674276` | Closed-loop evidence dispatch is proven |
| `26340674276` | recorded replay parity | Succeeded; `strict_parity_ready=true` with no shared-row mismatches | Dry-run/runtime parity proof is available for the checked recording slice |
| `26340673836` | runtime candidate replay | Succeeded but blocked; `updates_processed=43721`, `intents_submitted=0`, `orders=0`, `fills=0` | Current AutoFactor runtime candidate is not tradable and should be revised/rejected |
| `26344523079` | `main` at `a74f030b7e8c288144b5fd5a35a75da9b589bc78` | Hosted walk-forward succeeded; `persisted=true`; dispatched five runtime replay requests from `runtime_replay_requests` | Snapshot -> walk-forward -> durable trace -> runtime replay fan-out is working |
| `26344588318` / `26344588684` / `26344589064` / `26344589439` / `26344589743` | runtime candidate replay batch | All succeeded with `basis=runtime_market_update_replay`; trade counts were `29`, `29`, `8`, `16`, and `15`; fill rate was `1.0` for all | Runtime replay automation works, but every candidate remains below the 50-trade promotion gate |
| `26344749058` | `main` hosted walk-forward with replay `26344588318` | Succeeded; consumed true runtime replay evidence; closed-loop action became `fix_data` with zero replay requests | Promotion now sees runtime replay evidence and blocks on data/trade-count/settlement gates instead of runtime provenance |
| `26350607546` | `main` at `1281f567b6b5dfd9100d773b80555259ef43174d` | Research snapshot succeeded for `2026-05-17T00:00:00Z -> 2026-05-18T00:00:00Z`, `BTCUSDT,ETHUSDT,SOLUSDT`; data audit `ok`, gate mode `coverage`, `87616` observations, `35180` sampled PM book rows | The clean 24h sampled snapshot path is restored after the 2026-05-16 Binance LOB gap |
| `26350924329` | `main` at `1281f567b6b5dfd9100d773b80555259ef43174d` | Hosted walk-forward succeeded from snapshot `26350607546`; `persisted=true`; stages `factor_attribution,walk_forward`; dispatched five runtime candidate replay requests | Snapshot -> hosted walk-forward -> durable trace -> runtime replay fan-out works on current main |
| `26351022434` / `26351022707` / `26351022987` / `26351023244` / `26351023528` | runtime candidate replay batch | All succeeded with `basis=runtime_market_update_replay`, `51` trades, `51` unique events, `entry_fill_rate=1.0`, `roi=-0.125651`, total PnL `-96.123274`, and blockers `official_settlement_missing:48<51` plus `roi_too_low` | The chain now reaches executable replay trade count/fillability gates, but the candidates are economically negative and settlement coverage is incomplete |
| `26351190753` | replay-fed hosted walk-forward attempt | Failed before Rust: `end_date=2026-05-18` was parsed as `2026-05-19T00:00:00Z`, outside the one-day snapshot | Workflow date inputs are inclusive day strings; for this snapshot the valid 24h window is `2026-05-17` to `2026-05-17` |
| `26351209485` | replay-fed hosted walk-forward with replay `26351022434` | Succeeded, but correctly reported target mismatch because the replay was generated for `tradeable_full_depth_settlement_pnl` and the run used default `full_depth_settlement_executable_pnl` | Promotion/replay contracts fail closed when target names do not match |
| `26351351355` | replay-fed hosted walk-forward with matching `tradeable_full_depth_settlement_pnl` target | Succeeded; `persisted=true`; closed-loop `decision=fix_data`; no runtime replay requests; blockers are sampled execution surface, `official_settlement_missing:48<51`, and `roi_too_low:-0.125651<0.000000` | The chain now consumes true runtime replay evidence and routes to data repair instead of repeating replay fan-out |
| `26351573860` | full-depth execution-surface collection | Succeeded; `schema_version=full_depth_execution_surface.v1`; `full_fidelity=true`; `checked_hours=24`; `existing_hours=24`; `row_count=2214371` | Raw full-depth CLOB archive evidence exists for the checked 24h window |
| `26351573853` | official settlement repair dry-run | Succeeded; `candidate_market_count=1097`; `would_settle_count=2194`; `error_count=0`; `dry_run=true` | Settlement coverage can likely be repaired, but execute mode is a DB mutation requiring explicit ACK |
| `26352016263` | `main@a3e43c6a` replay-fed hosted walk-forward with runtime replay plus full-depth surface proof | Succeeded; `source_snapshot_contract.blocking_risk_flags=[]`; `satisfied_execution_surfaces=["clob_orderbook_snapshots"]`; closed-loop `decision=fix_data`; blockers remain official settlement and negative ROI | Full-depth handoff wiring is fixed; strategy promotion remains correctly blocked on settlement and economics |
| `26362501135` | `main@b7e75f5` replay-fed hosted walk-forward with matching target lane | Succeeded; consumed `runtime_market_update_replay` `26355035577`; `entry_fill_rate=1.0`; `53` trades; ROI `-0.079091`; `source_snapshot_contract.blocking_risk_flags=[]`; persisted trace | Research chain is restored through replay/full-depth evidence; remaining blocker is strategy economics, not data plumbing |
| `26362759645` | `main@b7e75f5` Research Trace Plan | Succeeded; `theme=revise_prior`; `candidate_count=8`; only blocker action `strategy_economics -> mutate_or_reject_negative_runtime_edge` | Durable trace planning no longer regresses to stale `fix_data` blockers |
| `26363545147` | `main@f887d944` Research Manager executor | Succeeded; dispatched child hosted walk-forward `26363548647`; executor artifact carried replay `26355035577`, full-depth proof from `26362501135`, and target `tradeable_full_depth_settlement_pnl` | Executor can now infer hidden replay/full-depth artifact evidence from trace-plan input |
| `26363548647` | `main@f887d944` hosted walk-forward from executor | Succeeded; downloaded candidate replay and full-depth proof; persisted trace; candidate replay remained `basis=runtime_market_update_replay`, `53` trades, `entry_fill_rate=1.0`, ROI `-0.079091`, blocker `roi_too_low` | Automatic closed-loop research works end-to-end and correctly rejects the current negative-ROI candidate |
| `26363701280` | `main@f887d944` Research Trace Plan after executor child | Succeeded; `theme=revise_prior`; `candidate_count=8`; only blocker action `strategy_economics -> mutate_or_reject_negative_runtime_edge` | Post-executor durable trace still classifies the next step as prior revision, not data/runtime repair |

## Repository Cleanup Evidence

| PR | Merge commit | Scope | Meaning |
| --- | --- | --- | --- |
| `#643` | `54acdf3041a686ac0b4211705abb8cfa9ec9d404` | Archived duplicate research/debug collectors and manual root-runtime deploy helpers | Active scripts no longer present these legacy paths as current research or deployment entrypoints |
| `#644` | `a913efd2e165b430a1096c4385149bf86d375cac` | Archived public-profile copycat and reverse-engineered dry-run prototypes | Profile scraping prototypes are no longer active strategy research or dry-run surfaces |
| `#645` | `1126c2d1fb6e3bbaafb884b67b8c9396667c7367` | Moved maintenance/watchdog ownership into platform release and archived `install-service.sh` | Canonical platform deploy preserves host-support timers without the legacy installer |

The current walk-forward/runtime replay evidence is deliberately blocked:

- `sweep-summary.json` still reports `decision=blocked`.
- Candidate replay is no longer only a diagnostic aggregate: the fresh batch
  uses `basis=runtime_market_update_replay`, reaches `51` trades on `51`
  distinct events, and reports `entry_fill_rate=1.0`. It remains blocked by
  `official_settlement_missing:48<51` and
  `roi_too_low:-0.125651<0.000000`.
- Snapshot contract blockers include
  `sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots`,
  so the run cannot make full-depth executable handoff claims.
- The snapshot backing this run contains `BTCUSDT`, `ETHUSDT`, and `SOLUSDT`;
  earlier six-symbol dispatch was invalid for that snapshot scope.

## Remaining Problems

1. **The automated research loop is proven, but strategy discovery is not.**

   `research_trace_plan` produces a typed plan from DB trace, and
   `research-manager-execute-plan.yml` has executed bounded follow-up actions.
   Hosted walk-forward now turns positive runtime-mapped rows into bounded
   `runtime-candidate-replay.yml` requests without manual artifact
   interpretation. After PR `#673`, the executor also carries prior runtime
   replay and full-depth proof into the next hosted walk-forward without hidden
   manual inputs. The current result is still not a dry-run handoff: the latest
   replay-fed candidate has `53` trades, complete fills, complete settlement
   rows, and `entry_fill_rate=1.0`, but ROI is `-0.079091` with total PnL
   `-62.877407450996`. The remaining discovery gap is finding a different
   profitable runtime-mappable factor, not lowering fillability or replay
   gates.

   Current answer to "can it automatically research/backtest/discover
   strategies": it can automatically produce factor attribution, persist trace,
   plan the next action, execute bounded follow-up research, consume runtime
   replay/full-depth evidence, and reject weak candidates. It has not yet found
   an automatically tradable strategy because the current candidate is negative
   ROI under executable runtime replay.

2. **Feature snapshots are still sampled research products, but full-depth
   execution proof is now consumable.**

   Snapshot manifests carry source surfaces, cadence, `raw_full_fidelity`,
   `snapshot_sampled`, and `gate_category`. AutoFactor promotion and aggregate
   candidate replay consume that manifest and now also consume verified
   `full_depth_execution_surface.v1` proof. A valid proof removes the sampled
   CLOB execution-surface blocker only for the matching covered window; invalid
   or incomplete proof remains fail-closed.

   The remaining data-layer gap is making this full-depth proof a normal
   trace-attached input for every executable handoff, not an optional follow-up
   artifact that must be discovered run by run.

3. **Runtime input canonicalization is now gated for AutoFactor promotion/replay,
   but not yet a shared cross-language source of truth.**

   Factor registry contracts now carry typed runtime metadata, and Python
   promotion/replay consumers fail closed when registry previews are present
   but contracts are missing, blocked, or semantically mismatched. The remaining
   gap is deeper: Rust runtime scoring and Rust/Python contract catalogs still
   need one generated/shared source for feature names, horizons, LOB surfaces,
   blocker semantics, and strategy profile/family mapping.

4. **Multi-horizon label/accounting has a shared AutoFactor catalog, but the
   broader label engine still needs generated contracts.**

   AutoFactor promotion now checks that the candidate runtime replay tape's
   `source_factor.target/horizon` matches the factor row being promoted.
   Runtime/aggregate replay artifacts write the same target/horizon into their
   decision contract, and AutoFactor target/horizon/accounting metadata now
   comes from `config/autofactor_accounting_catalog.json`. The remaining
   architecture gap is consolidating 30s / 60s / 5m / 15m label definitions
   beyond AutoFactor into one generated catalog shared by Rust research, runtime
   replay builders, and persistence.

5. **DuckDB should remain a query accelerator, not durable state.**

   Durable research registry, promotion decisions, replay tape identity, and
   experiment trace belong in PostgreSQL plus immutable artifacts/lake files.
   DuckDB can be used for local Parquet scans, but not as the authoritative
   strategy discovery state layer.

6. **Legacy archives are no longer a second runnable architecture, but some
   active compatibility collectors and repair jobs still need ownership
   decisions.**

   The retired archived executable assets have been removed. The remaining
   active Python/shell helpers are not all wrong: some are still deployed
   collector/repair surfaces, while others are prototype paths. Manual
   direct-DB factor research is no longer an active break-glass entrypoint:
   factor review and walk-forward are snapshot-only active examples, and
   `scripts/run_factor_research*.sh` have been retired. The LOB TCN trainer
   remains active only while the ML lane still references it; retire it after a
   replacement or explicit owner exists.

## Required Next Work

| Priority | Work | Done when |
| --- | --- | --- |
| P0 | Keep official settlement completeness as a hard runtime replay gate | Every replay-fed handoff proves official settlement for all traded events, and missing coverage routes to settlement repair instead of economics analysis |
| P0 | Attach full-depth execution-surface proof to every executable handoff | Hosted walk-forward, standalone promotion, trace persistence, and Research Manager plans all carry the verified full-depth proof without manual run-id stitching |
| P0 | Keep Research Manager candidate replay contract-driven | Executor replays only explicit or trace-derived unblocked runtime contracts and fails closed without one |
| P0 | Convert negative runtime economics into stronger next search mutations or rejection | Persistent `roi_too_low` replay evidence changes the typed prior search space or closes the candidate family instead of rediscovering equivalent formulas |
| P1 | Promote runtime input canonicalization to a generated shared contract | Rust runtime scoring, Rust alpha-search, and Python promotion/replay use one source of truth instead of mirrored catalogs |
| P1 | Complete full-depth executable evidence layer | Runtime replay and full-depth lake evidence become first-class queryable trace objects, not only workflow artifacts |
| P1 | Generate shared non-AutoFactor label contracts | Runtime, research, replay, and trace persistence derive 30s / 60s / 5m / 15m label definitions from one source |
| P1 | Finish remaining active compatibility ownership decisions | One-shot repairs, prototype trainers, and compatibility collectors are either proven active, moved behind stronger ownership gates, or replaced |

## Verdict

The architecture is now much cleaner than the mixed state: snapshot artifacts,
factor attribution, durable trace, candidate replay, and diagnostic backtest
have separate evidence stages and the old direct-DB factor execution branch has
been cut out of the active workflows.

The research chain has been restored through durable trace planning, but not
through profitable strategy discovery or dry-run handoff. The proven sequence is:

```text
research-snapshot.yml
  -> factor-walk-forward-v2-hosted-artifact.yml
  -> persist Research OS trace
  -> closed-loop runtime replay request batch
  -> research-trace-plan.yml
  -> research-manager-execute-plan.yml
  -> runtime-candidate-replay.yml / recorded-replay-parity.yml
```

The missing sequence is now:

```text
blocked runtime candidate
  -> closed-loop fix_data for settlement/full-depth evidence gaps
  -> closed-loop prior revision / new runtime-mappable candidate if ROI stays negative
  -> runtime_market_update_replay with at least 50 distinct event trades
  -> positive executable ROI and complete official settlement coverage
  -> handoff only if full-depth execution, official settlement, ROI, fillability, and parity gates pass
```

The system can now automatically research, persist trace, dispatch replay/parity
evidence, carry replay/full-depth proof into the next hosted search, and reject
a weak candidate. The latest evidence moved the blocker from "chain plumbing"
to "negative executable ROI." It should not be described as having found an
automatically tradable strategy yet.
