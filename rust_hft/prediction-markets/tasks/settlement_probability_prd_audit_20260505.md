# Settlement Probability PRD Completion Audit

Date: 2026-05-05

## Objective

Complete the Polymarket Crypto 5m / 15m settlement-probability strategy PRD,
run the required tests, and decide whether the data and strategy evidence are
sufficient for replay / dry-run promotion.

The strategy gate is:

```text
q_side - full_depth_entry_sweep_avg_price
  > fee + slippage + latency + model_error + safety_margin
```

Top-book evidence is not decision-grade. The promotion path requires
snapshot-backed full-depth execution labels and probability-model validation.

## Current Branch And PR Evidence

- Branch: `feat/settlement-probability-prd`
- PR: #319
- Latest reviewed head: `046291b8` after the settlement walk-forward report,
  PRD promotion gate, replay-parity artifact wiring, and repeatable gate
  orchestrator
- PR status: mergeable
- Ordinary PR CI: passed on run `25354110281`
- PR Auto Review: passed on run `25354110277`
- CodeRabbit: success
- Runner blocker issue: #320, closed after `ploy-ci-1` recovery
- Follow-up issue for remaining PRD promotion blockers: #321
- Final pushed head: `046291b8`
- Final PR check status: all required PR checks green on run `25360108368`
  after rerunning the flaky control-plane/core job; PR Auto Review
  `25360108349`; CodeRabbit success.
- Latest short-window promotion-gate smoke: Factor Walk-Forward V2 run
  `25359476569`, source snapshot `25356726430`, completed successfully and
  printed `ready_for_dry_run_handoff=false`.
- Final runner status after cancelling the diagnostic snapshot:
  `ploy-ci-1` is `online` / `busy=false`.

## Prompt-To-Artifact Checklist

| PRD requirement | Required artifact / command | Current evidence | Status |
| --- | --- | --- | --- |
| Main lane is Settlement Probability Trading | `tasks/todo.md`, PR #319 code path | Plan records settlement probability as the main lane and repricing as secondary diagnostic | Complete |
| Use full Polymarket CLOB execution, not top book | `FullDepthExecutionMatrix`, full-depth labels in `crates/ploy-research/src/factors_v2.rs` | Snapshot-backed reports `25353780686` / `25353780673` include full-depth entry-fillable settlement rows | Complete |
| Candidate rows are side + stake + executable labels | `factor_walk_forward_v2` report inputs | Reports evaluate side-aligned candidate rows at `15u` stake with full-depth settlement labels | Complete |
| Probability baselines | Settlement probability report | Reports include `q_naive_50_50`, `q_market_midpoint`, distance Phi, LOB/vol baselines, existing fair/model probabilities | Complete |
| Calibration report | `=== Settlement Probability Report ===` artifacts | Present in runs `25353780686` and `25353780673` | Complete |
| Edge bucket report | Factor walk-forward artifact `report.txt` | Present in runs `25353780686` and `25353780673` | Complete |
| Baseline comparison | Settlement probability report baseline section | Present in runs `25353780686` and `25353780673` | Complete |
| Anti-overfit diagnostics | label shift, prediction shift, symbol holdout, baseline ablation sections | Present in runs `25353780686` and `25353780673` | Complete |
| EventVolSurface / empirical prior | `q_event_surface_empirical` in settlement probability report | Implemented non-leaky event-surface prior; current event is excluded from bucket/global priors | Complete |
| Final probability blend | `q_final_logit_blend` in settlement probability report | Combines market midpoint, distance/LOB/vol probability, and EventVolSurface prior in logit space | Complete |
| Walk-forward OOS | `=== Settlement Probability Walk-Forward Report ===` | Implemented train-window-only EventVolSurface OOS report; short-window smoke intentionally has no non-empty OOS windows | Code complete; decision-grade evidence pending |
| PRD promotion gate | `=== Settlement Probability PRD Promotion Gate ===` | Run `25359476569` prints `ready_for_dry_run_handoff=false`, `walk_forward_oos=false`, and `recorded_replay_parity=false` | Complete as a blocker gate |
| Replay parity artifact input | `factor_walk_forward_v2 --replay-parity-json`, workflow `options_json.replay_parity_json`, `options_json.replay_parity_run_id` | The report can now consume `scripts/replay_dryrun_parity.py` JSON either from a runner path or from a downloaded `replay-dryrun-parity-*` artifact | Code complete; no passing parity artifact yet |
| Repeatable PRD gate runner | `scripts/run_settlement_probability_prd_gate.py` | Script dispatches strict `pm5d-vol` snapshot with `data_gate=critical` / `upload_full_snapshot=true`, waits, then dispatches snapshot-backed `factor-walk-forward-v2` with optional replay parity artifact linkage | Complete as orchestration; decision-grade data still pending |
| Manual CI PRD gate entrypoint | `.github/workflows/settlement-probability-prd-gate.yml` | Workflow-dispatch wrapper runs the same strict orchestrator from GitHub Actions and fails closed when the promotion gate is blocked | Complete as orchestration; decision-grade data still pending |
| Official settlement fidelity | Snapshot manifest `require_official_settlement=true` | Provenance for snapshots `25254380121` and `25255158983` confirms official settlement required | Partial evidence |
| Data quality / coverage | `data-gap-audit.md` | Strict `pm5d-vol` run `25354264444` failed at `Audit required market data`; every required source had critical max gaps around `1700m` | Failing / blocks promotion |
| Deribit / vol inputs | `data_profile=pm5d-vol` or `include_deribit=true` snapshot | Short-window snapshot `25356726430` includes Deribit; strict retained 168h snapshot still fails coverage | Partial; full retained evidence missing |
| Portable replay evidence | Full research snapshot artifact | Short-window artifact `research-snapshot-25356726430` is portable and replayable; strict retained full snapshot is still missing | Partial; full retained evidence missing |
| Replay parity | `ReplayParityReport` / recorded replay artifacts | Promotion gate now exposes `recorded_replay_parity=false`; no parity artifact has passed | Missing / blocks promotion |
| Dry-run readiness | Dry-run handoff packet and kill switch evidence | PRD gates have not passed | Not ready |

## Remote Validation Evidence

The original queued runs failed before data execution because they referenced an
unreachable commit and then, after retriggering, used an end-date input that
expanded beyond the immutable snapshot window. The decision-grade reruns used
`end_date=2026-05-01`, which the CLI expands to the snapshot window
`2026-04-21 00:00:00 UTC -> 2026-05-02 00:00:00 UTC`.

Successful snapshot-backed runs:

- BTC/ETH/SOL: run `25353780686`, artifact
  `factor-walk-forward-v2-25353780686`
- XRP/DOGE/BNB: run `25353780673`, artifact
  `factor-walk-forward-v2-25353780673`

Execution evidence:

- BTC/ETH/SOL: full-depth entry fill rate `49.23%`, exit fill rate `40.70%`,
  full-depth settlement PnL rows `112256`.
- XRP/DOGE/BNB: full-depth entry fill rate `45.22%`, exit fill rate `36.27%`,
  full-depth settlement PnL rows `97939`.

Settlement probability evidence:

- BTC/ETH/SOL `q_market_midpoint`: ECE `0.004096`, top-edge full-depth
  settlement PnL `+4.9364`, profit factor `0.9072`, symbol holdouts all pass
  (`BTC +7.5521`, `ETH +4.8017`, `SOL +1.6634`).
- BTC/ETH/SOL `q_existing_fair_prob`: ECE `0.003891`, top-edge full-depth
  settlement PnL `+4.7994`, profit factor `0.9071`, symbol holdouts all pass
  (`BTC +7.5528`, `ETH +4.3742`, `SOL +1.6950`).
- XRP/DOGE/BNB `q_existing_fair_prob`: ECE `0.013897`, top-edge full-depth
  settlement PnL `+0.5916`, but DOGE holdout fails (`DOGE -0.2531`).
- XRP/DOGE/BNB `q_market_midpoint`: ECE `0.014006`, top-edge full-depth
  settlement PnL `+0.5490`, but DOGE holdout fails (`DOGE -0.1951`).

Interpretation:

- BTC/ETH/SOL has a real settlement-probability candidate worth the next
  replay-quality research pass.
- XRP/DOGE/BNB is not an all-symbol candidate; at most XRP/BNB deserve a
  narrowed follow-up, while DOGE should be excluded or separately modeled.
- Distance-only Phi baselines are anti-signaled in these reports and must not
  be promoted as the settlement model.

## Snapshot Artifact Audit

Existing snapshot run artifacts are not enough to complete the PRD gate without
the runner-local registry:

- `25254380121`: `research-snapshot-provenance-25254380121`, about 13KB
- `25255158983`: `research-snapshot-provenance-25255158983`, about 13KB
- Both say `full_snapshot_embedded=false`
- Both say `registry=runner-local`
- Downloaded contents do not include `observations.json`,
  `pm_book_snapshots.json`, `deribit_snapshots.json`, or parquet observations

The provenance is useful for row counts and audit status, but it is not a
replayable dataset.

## Data Sufficiency Finding

Data sufficiency is not established.

Available provenance shows substantial row counts:

- BTC/ETH/SOL: observations `114008`, PM books `171794`
- XRP/DOGE/BNB: observations `108296`, PM books `141692`

But the same provenance reports:

- `data_audit_status=critical`
- max gaps around 280-410 minutes across required sources
- Deribit excluded (`include_deribit=false`)
- no portable full snapshot artifact

Therefore current evidence is enough to identify the next candidate lane, but
not enough for dry-run promotion. BTC/ETH/SOL `q_market_midpoint` /
`q_existing_fair_prob` top-edge settlement probability is the strongest
candidate. Promotion remains blocked by critical data gaps, missing Deribit vol
inputs in a retained strict window, missing non-empty settlement-probability OOS
windows, and missing recorded replay parity.

Fresh strict data-gate attempt:

- Run: `25354264444`
- Ref: `feat/settlement-probability-prd`
- Profile: `pm5d-vol`
- Gate: `critical`
- Window: `2026-04-21..2026-05-01`
- Symbols: `BTCUSDT,ETHUSDT,SOLUSDT`
- Stake: `15u`
- Result: failed at `Audit required market data`
- Required sources: `polymarket_quotes`, `polymarket_orderbooks`,
  `deribit_iv`, `deribit_atm_greeks`, `binance_price`,
  `binance_agg_trades`, `binance_lob`
- Gap evidence: PM quotes/orderbooks max gap `1705m`; Deribit and Binance
  sources max gap around `1700m`
- Root-cause follow-up on `tango-1-1` found this was a real continuous
  collection hole, not a missing "7 days" misunderstanding:
  - strict audit gap: PM quotes/orderbooks from about
    `2026-05-04 05:29/05:30 +0800` to
    `2026-05-05 09:54/09:55 +0800`;
  - Deribit/Binance sources from about `2026-05-04 05:30 +0800` to
    `2026-05-05 09:50 +0800`;
  - Binance price / aggTrade / LOB services had `NRestarts=6480`, and journals
    showed repeated `timed out during opening handshake` failures until around
    `2026-05-05 09:50 +0800`;
  - Deribit IV stayed active but logged repeated upstream SSL/read-timeout
    errors, and ATM Greeks cycles returned `ok=0/0` while no fresh IV
    instruments were available;
  - current one-hour audit after recovery is `ok` for PM quotes/orderbooks,
    Deribit IV/Greeks, and BTC/ETH/SOL Binance price/trades/LOB.

This is a valid negative PRD result, not a code failure. It means the current
database window cannot prove the settlement-probability system is ready for
replay or dry-run under the full PRD.

Diagnostic follow-up:

- Run: `25354314443`
- Difference: `data_gate=never`, `upload_full_snapshot=true`
- Status: completed with conclusion `cancelled`; it was cancelled while still
  in `Compile snapshot` after the strict decision-grade data gate had already
  failed
- Interpretation: useful only to inspect materialized rows and artifact shape;
  it cannot override strict data insufficiency.

Conservative execution surface:

- `factor_walk_forward_v2` now prints a second Full-Depth Execution Matrix with
  `visible_depth_haircut=0.5` and `max_levels=3`.
- The settlement probability report now also prints conservative settlement PnL
  and conservative profit-factor columns for baseline, calibration, and edge
  buckets using the same `50% visible depth / max 3 CLOB levels` assumption.
- Verification:
  `rustfmt --edition 2021 --check
  crates/ploy-research/examples/factor_walk_forward_v2.rs`
- Verification:
  `CARGO_TARGET_DIR=/tmp/ploy-settlement-conservative-matrix
  /opt/homebrew/bin/timeout 240 rtk cargo check -p ploy-research --example
  factor_walk_forward_v2 --features db --no-default-features`
- Remaining conservative gap: conservative capacity/fillability and conservative
  settlement edge buckets now exist, but they still need a decision-grade
  retained snapshot plus recorded replay parity before dry-run handoff.

## Short-Window Workflow Smoke

The user explicitly allowed missing history to keep filling while using current
or prior data for a short test. This smoke is not promotion evidence; it only
checks that the PRD workflow can run over a portable full snapshot after the
collectors recovered.

Snapshot smoke:

- Run: `25356726430`
- Ref: `feat/settlement-probability-prd`
- Profile: `pm5d-vol`
- Gate: `critical`
- Audit lookback: `1h`
- Window input: `2026-05-05..2026-05-05`, parsed as
  `2026-05-05 00:00:00 UTC -> 2026-05-06 00:00:00 UTC`
- Symbols: `BTCUSDT,ETHUSDT,SOLUSDT`
- Stake: `15u`
- Artifact: `research-snapshot-25356726430`
- Result: success
- Rows: observations `1225`, Deribit snapshots `485`, PM book snapshots `2876`
- Data audit: `ok` for PM quotes/orderbooks, Deribit IV/Greeks, and
  BTC/ETH/SOL Binance price/trades/LOB in the one-hour audit window
- Official settlement required: `true`
- Deribit included: `true`

Downstream walk-forward smoke:

- First run: `25356886955`
- Result: failed at snapshot read before reporting
- Root cause: portable `observations.json` serialized non-finite
  `flip_age_secs` as JSON `null`, while the loader expected a concrete `f64`
- Fix: commit `5677f294` makes `flip_age_secs` deserialize nullable f64 values
  as `NaN` and extends the snapshot-null regression test
- Verification: local snapshot replay against `research-snapshot-25356726430`
  completed and printed the settlement probability report
- Re-run: `25357077864`
- Result: success
- Artifact: `factor-walk-forward-v2-25357077864`
- Rows: source observations `1225`, side rows `2450`, executable settlement
  PnL rows `1004`, Deribit-enriched rows `1618`
- Execution: full-depth entry fill `40.98%`, exit fill `39.35%`
- Baseline highlights:
  - `q_existing_fair_prob`: ECE `0.101327`, avg full-depth settlement PnL
    `+1.2079`, avg conservative settlement PnL `+0.6244`, top-edge full-depth
    settlement PnL `+9.3541`, top-edge conservative PnL `+8.0545`
  - `q_market_midpoint`: ECE `0.102095`, avg full-depth settlement PnL
    `+1.2079`, avg conservative settlement PnL `+0.6244`, top-edge full-depth
    settlement PnL `+5.3665`, top-edge conservative PnL `+3.3891`
  - `q_distance_lob_vol_phi`: ECE `0.059314`, but average full-depth
    settlement PnL `-0.9257`, so lower calibration error did not translate
    into positive executable EV in this smoke
- Holdout caveat: SOL holdout is negative for `q_existing_fair_prob`
  (`-0.3116`) and `q_market_midpoint` (`-0.2303`)

Promotion-gate smoke after the latest report changes:

- Run: `25359476569`
- Source snapshot: `research-snapshot-25356726430`
- Result: success
- Report confirms `=== Settlement Probability PRD Promotion Gate ===`
- Gate output: `ready_for_dry_run_handoff=false`
- Blocking rows visible in the artifact:
  - `walk_forward_oos=false`, because the one-day smoke cannot form non-empty
    train+test OOS windows
  - `recorded_replay_parity=false`, because no recorded replay parity artifact
    was supplied

Replay-parity input integration after latest changes:

- `factor_walk_forward_v2` accepts optional `--replay-parity-json <path>`.
- `.github/workflows/factor-walk-forward-v2.yml` accepts optional
  `options_json.replay_parity_json` and forwards it to the example.
- `.github/workflows/factor-walk-forward-v2.yml` also accepts optional
  `options_json.replay_parity_run_id` and
  `options_json.replay_parity_artifact_name`; when supplied, it downloads the
  `replay-dryrun-parity-*` artifact and forwards
  `artifacts/replay-parity/parity-evaluation.json` to the example.
- The parser reads `runtime_evidence_comparison.strict_parity_ready`,
  `event_comparison.strict_parity_ready`, `risk_flags`, and `decision` from
  `scripts/replay_dryrun_parity.py` output. The promotion gate passes recorded
  replay parity only when runtime parity and event parity are true and risk
  flags are empty.

Repeatable gate orchestration after latest changes:

- `scripts/run_settlement_probability_prd_gate.py` is the canonical local
  control-plane helper for the remaining PRD evidence chain.
- It dispatches `research-snapshot.yml` with `data_profile=pm5d-vol`,
  `data_gate=critical`, `upload_full_snapshot=true`, and the requested
  symbols/stake/window.
- It waits for the strict snapshot; if the data gate fails, it comments issue
  #321 and exits blocked instead of running downstream reports on invalid data.
- If the snapshot succeeds, it dispatches `factor-walk-forward-v2.yml` with the
  produced `snapshot_run_id` and optional replay parity artifact linkage.
- Dry-run validation of the dispatch shape passed for a one-hour smoke command:

```bash
scripts/run_settlement_probability_prd_gate.py \
  --start-date 2026-05-05 \
  --end-date 2026-05-05 \
  --audit-lookback-hours 1 \
  --dry-run
```
- First live dispatch exposed a workflow-definition ref bug: `gh workflow run`
  used the repository default branch workflow definition even though the
  workflow input `git_ref` checked out the PR branch inside the job. Run
  `25360488853` therefore failed before data audit because the default-branch
  `research-snapshot.yml` did not yet know `upload_full_snapshot`. The
  orchestrator now passes `--ref <git_ref>` to `gh workflow run`, so the
  workflow definition and checkout ref stay aligned.
- Second live dispatch after the `--ref` fix completed the full short-window
  chain:
  - Snapshot run `25360564148`: success, `data_audit_status=ok`,
    `include_deribit=true`, `upload_full_snapshot=true`, rows
    observations `2857`, Deribit `1038`, PM books `6490`, official settlement
    required.
  - Walk-forward run `25360794716`: success from snapshot run `25360564148`,
    source observations `2857`, side rows `5714`, full-depth PnL rows `4712`,
    Deribit-enriched rows `3894`.
  - Promotion gate remained blocked as intended:
    `ready_for_dry_run_handoff=false`,
    `anti_overfit_diagnostics=false`,
    `walk_forward_oos=false`,
    `recorded_replay_parity=false`.
- The orchestrator now downloads the `factor-walk-forward-v2-*` artifact after
  a successful walk-forward run, parses the `Settlement Probability PRD
  Promotion Gate`, comments the exact blocked gates, and exits blocked when
  `ready_for_dry_run_handoff=false`. A successful workflow run is no longer
  treated as a successful promotion gate by itself.
- `.github/workflows/settlement-probability-prd-gate.yml` is now the canonical
  manual GitHub Actions entrypoint for running the same strict gate from CI.
  It accepts the retained data window, symbols, stake, and optional replay
  parity artifact run id. A failed workflow can be a correct PRD result when
  the downstream promotion gate remains blocked.
- Merged-entrypoint smoke on `main`:
  - Settlement Probability PRD Gate run `25361728377` invoked the new manual
    wrapper with `audit_lookback_hours=1`.
  - Snapshot run `25361734673` succeeded with `pm5d-vol`,
    `data_gate=critical`, full snapshot upload, and official settlement
    required.
  - Factor Walk-Forward V2 run `25361999475` succeeded from that snapshot.
  - Parent workflow exited `3` intentionally because
    `ready_for_dry_run_handoff=false`; blockers remained
    `anti_overfit_diagnostics`, `walk_forward_oos`, and
    `recorded_replay_parity`.

Interpretation:

- The short-window workflow chain now works: strict current data audit,
  portable full snapshot artifact, full-depth execution matrix, Deribit-included
  settlement probability report, conservative edge columns, promotion blocker
  gate, and issue-comment evidence.
- This does not unblock dry-run/live. The sample is tiny and not a retained
  clean 168h window, the symbol holdout is not stable, walk-forward OOS is
  empty, and recorded replay parity is still missing.

## Current Continuation State

Checked again on 2026-05-05 14:59 CST after the PRD gate wrapper landed on
`main`.

- The correct next retained-data gate is still the strict `168h` gate, but it
  should not be rerun immediately. The collector recovery evidence above places
  the relevant data hole at roughly 2026-05-04 05:30 CST to 2026-05-05
  09:50/09:55 CST, so any 168-hour audit run before roughly 2026-05-12
  09:55 CST still includes the known outage and is expected to fail for data
  quality rather than model quality.
- The short-window PRD gate has already proven the control-plane shape on
  `main`: strict `pm5d-vol` snapshot, full snapshot upload, Factor
  Walk-Forward V2, full-depth/conservative labels, promotion-gate parsing, and
  issue evidence all work.
- The remaining non-data blocker that can be worked before the clean 168-hour
  window is recorded replay/dry-run parity. The current
  `replay-dryrun-parity.yml` workflow requires both a replay/backtest artifact
  and a real dry-run JSON report URL with strict comparable order/fill/event
  fields. No passing parity artifact exists yet, so this gate must remain
  blocked instead of being hand-filled.
- Do not create the settlement dry-run handoff packet until the promotion gate
  returns `ready_for_dry_run_handoff=true`; the next handoff would otherwise be
  based on short-window smoke rather than PRD-grade evidence.

## Recovery Checklist

1. Collect or compile a fresh snapshot with `data_gate=critical` and
   `upload_full_snapshot=true`, so the next evidence is portable and not only
   runner-local.
2. Run a `pm5d-vol` / Deribit-included snapshot for BTC/ETH/SOL to satisfy the
   PRD volatility lane.
3. Run the conservative settlement edge buckets against the next strict
   snapshot and add any remaining quote-age, latency, and adverse-selection
   buffers needed for live handoff.
4. Narrow the candidate to BTC/ETH/SOL settlement probability top-edge buckets.
5. Exclude DOGE from any all-six settlement promotion until its holdout turns
   positive under the same full-depth labels.
6. Run recorded replay parity with the same scorer before any dry-run handoff.

## Completion Decision

The PRD is not complete as a tradable strategy system yet.

Completed as code/report architecture:

- full-depth execution truth;
- settlement probability baselines and q-final blend;
- calibration, edge-bucket, baseline, anti-overfit, symbol-holdout, and
  walk-forward report surfaces;
- conservative CLOB haircut labels and conservative settlement PnL columns;
- explicit PRD promotion gate;
- replay parity artifact input path;
- repeatable strict snapshot -> walk-forward gate orchestration.

Still blocking strategy completion:

- no clean retained `pm5d-vol` snapshot with `data_gate=critical`;
- no decision-grade non-empty OOS window from that clean snapshot;
- no passing recorded replay/dry-run parity artifact;
- no dry-run handoff packet with fixed stake, strict kill switch, and shared
  scorer parity.

Therefore the current correct state is: PR #319 is mergeable as PRD
infrastructure, but the strategy is not yet approved for dry-run/live.
