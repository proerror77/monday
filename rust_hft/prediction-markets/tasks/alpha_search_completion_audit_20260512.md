# Alpha Search Completion Audit - 2026-05-12

## Objective

Build a system that can automatically generate and explore alpha factors, then
produce a strategy that is eligible for profitable dry-run promotion.

This audit intentionally separates discovery evidence from promotion evidence.
Alpha-search reward, MCTS rank, or a passing candidate row is not enough to
claim a profitable strategy.

## Current Verdict

Status: `partially complete`.

2026-05-13 update: the previous ready dry-run handoff is not currently a
profitable strategy. The live dry-run evidence for
`pm5d.threelayer.settlement-probability-btc-eth.dryrun` was negative, and old
runtime rows had to be cleared before any new dry-run performance claim could be
made. Reset workflow run `25754514193` executed after the dry-run deployment was
paused, backed up and deleted only the scoped runtime rows, and the deployment
was resumed after clean-baseline verification.

The repo can automatically generate and explore alpha candidates through the
hosted Factor Walk-Forward V2 path, and run `25687766026` produced a ready
dry-run handoff. PR `#433` merged that handoff into the
settlement-probability BTC/ETH dry-run config. This is a promotable dry-run
candidate, not a proven profitable strategy.

The remaining blocker is post-merge dry-run deployment and fresh executable
evidence for:

`pm5d.threelayer.settlement-probability-btc-eth.dryrun`

2026-05-13 later update: the system gap is now narrower. PR `#484` added
entry-price-quality as a bounded AutoFactor research feature, generated
settlement candidates, runtime scorer suffixes, promotion mappings, and typed
prior `tasks/alpha_search_priors/pm5d_entry_price_quality_prior_20260513.json`.
PR `#485` made the 6-hour full scheduled market-data audit use coverage mode.
These fix search/promotion plumbing, not profitability. Current promotion
remains blocked by retained-window LOB coverage and insufficient fresh dry-run
sample size.

Latest deploy preflight:

- run: `25766143754`
- workflow: `deploy-tango-1-1.yml`
- git ref: `main@f26490541d82127db78512e6a186bd84117c5340`
- input: `git_ref=main`, `deploy=false`
- status: `completed`
- result: build-only success
- interpretation: latest `main` builds the deploy bundle and passes the bundle
  guard proving `pm5d.threelayer.live` remains paused in the shipped
  deployment config. Because `deploy=false`, OSS upload, SSH, Cloud Assistant,
  remote restart, and service mutation steps were skipped.

Earlier deploy preflight:

- run: `25688999691`
- workflow: `deploy-tango-1-1.yml`
- input: `git_ref=main`, `deploy=false`
- status: `completed`
- result: build-only success
- interpretation: latest `main` builds the deploy bundle and passes the bundle
  guard proving `pm5d.threelayer.live` remains paused in the shipped
  deployment config. No OSS upload, SSH, Cloud Assistant, remote restart, or
  service mutation was executed.

Latest read-only remote config check:

- checked_at: `2026-05-11T18:36:46Z`
- remote deployment state:
  `pm5d.threelayer.live desired=Paused observed=Paused`;
  `pm5d.threelayer.settlement-probability-btc-eth.dryrun desired=Running observed=Running`
- remote dry-run score:
  `autofactor_formula:auto_settlement_full_depth_settlement_edge`
- `origin/main` dry-run score:
  `autofactor_formula:auto_settlement_conservative_settlement_edge_x_near_strike`
- interpretation: protected dry-run deployment is still required because the
  running settlement-probability dry-run lane has not yet picked up the ready
  hosted handoff currently on `main`.

Latest reset / data-quality evidence:

- 24h data audit run: `25747004738`
  - `quick` 1h status: `ok`
  - 24h retained coverage: `critical`
  - blocker: `binance_lob/BTCUSDT` and `binance_lob/ETHUSDT` still contain the
    known 80-minute gap from `2026-05-12 09:28 +08` to `10:48 +08`
  - interpretation: do not run promotion-grade 24h/48h walk-forward over this
    retained window.
- 24h BTC/ETH coverage recheck run: `25753059613`
  - workflow: `market-data-gap-audit.yml`
  - input: `gate_mode=coverage`, `full_lookback_hours=24`,
    `symbols=BTCUSDT,ETHUSDT`, `fail_on=never`
  - quick 1h status: `ok`
  - full 24h retained coverage: `critical`
  - blocker: `binance_lob/BTCUSDT` and `binance_lob/ETHUSDT`, max gap
    `80m >= 15m`
  - interpretation: recent collector health is OK, but the retained 24h window
    is still not promotion-grade. The known LOB gap must age out before 24h+
    walk-forward/MCTS evidence can support promotion.
  - earliest clean-window timing if no new gaps appear:
    - 24h window after `2026-05-13 10:48 +08`
    - 48h window after `2026-05-14 10:48 +08`
    - 72h window after `2026-05-15 10:48 +08`
- 24h BTC/ETH coverage recheck run: `25758692951`
  - workflow: `market-data-gap-audit.yml`
  - git ref: `main@d43873a72cb9c8a51313b0f6a301c4dfe1bb1978`
  - input: `gate_mode=coverage`, `full_lookback_hours=24`,
    `symbols=BTCUSDT,ETHUSDT`, `fail_on=never`
  - quick 1h status: `ok`
  - full 24h retained coverage: `critical`
  - blocker: `binance_lob/BTCUSDT` and `binance_lob/ETHUSDT`, max gap
    `85m >= 15m`, from `2026-05-12 09:24 +08` to `10:49 +08`
  - interpretation: collector freshness is OK, but promotion-grade 24h
    walk-forward/MCTS remains blocked until this gap ages out.
- scheduled quick audit run: `25761849741`
  - workflow: `market-data-gap-audit.yml`
  - git ref: `main@7308aee535aa9b13d15230c625b9637c147e3429`
  - event: `schedule`
  - summary: `gate_mode=freshness`, `run_full=false`, quick 1h status `ok`
  - interpretation: the 30-minute scheduled collector-health path still works
    after PR `#485`. The next full scheduled coverage audit is the
    `17 */6 * * *` UTC path.
- reset preview run: `25748008417`
  - workflow: `reset-strategy-runtime-evidence.yml`
  - input: `execute=false`
  - matched rows: `185` orders, `20` fills
  - interpretation: old runtime evidence can be backed up and cleared, but no
    deletion was executed.
- guard verification run: `25748940434`
  - workflow: `reset-strategy-runtime-evidence.yml`
  - input: `execute=true`, `allow_running=false`
  - result: intentional failure before SSH/reset execution
  - artifact: `guard-status.json`
  - status: `blocked`
  - reason: `deployment_running`
  - desired/observed state: `running` / `running`
  - interpretation: the destructive reset is correctly blocked until the dry-run
    deployment is paused or stopped.
- dry-run candidate gate run: `25751440786`
  - workflow: `dryrun-candidate-gate.yml`
  - input: `mode=clean-baseline`
  - result: expected failure / blocked
  - artifact: `dryrun-candidate-gate-25751440786`
  - status: `blocked`
  - reason: `residual_runtime_evidence`
  - residual counts: `20` total trades, `20` closed trades, `185` total orders,
    `20` buy orders, `165` sell orders
  - interpretation: the CI gate now proves the current dry-run report is not a
    clean baseline and will block post-reset promotion until runtime evidence is
    actually cleared.
- dry-run candidate-quality gate run: `25753663370`
  - workflow: `dryrun-candidate-gate.yml`
  - input: `mode=candidate-quality`
  - result: expected failure / blocked
  - artifact: `dryrun-candidate-gate-25753663370`
  - status: `blocked`
  - failures: closed trades `20 < 50`, realized PnL `-115.17 < 0`,
    profit factor `0.2927 < 1.1`, max drawdown `-136.3244 < -50`
- reset execute run: `25754514193`
  - workflow: `reset-strategy-runtime-evidence.yml`
  - input: `execute=true`, `allow_running=false`,
    `confirm=delete-strategy-runtime-evidence`
  - guard artifact: `status=allowed`, deployment `desired_state=paused`,
    `observed_state=paused`
  - scope: `deployment_id=pm5d.threelayer.settlement-probability-btc-eth.dryrun`,
    `strategy_id=three_layer`, runtime modes `dry_run,dryrun,paper`
  - before reset: `185` orders, `20` fills
  - after reset: `0` orders, `0` fills
  - post-reset clean-baseline gate: `passed` with reason
    `target_strategy_absent`
  - post-reset deployment check: `desired_state=running`,
    `observed_state=running`, `deployment_state=enabled`
  - interpretation: contaminated dry-run evidence is cleared. Candidate-quality
    remains blocked until fresh post-reset closed trades accumulate.
- post-resume candidate-quality check:
  - checked after deployment resumed from reset run `25754514193`
  - fresh post-reset rows: `1` buy order, `1` open position, `0` closed trades
  - candidate-quality status: `blocked`
  - failures: closed trades `0 < 50`, profit factor `0.0 < 1.1`
  - interpretation: fresh collection has started, but there is still no
    post-reset closed-trade sample for profitability claims.
  - advisory: buy fill rate is `97.93%`, so fillability is not the main
    current blocker.
- dry-run candidate-quality gate run: `25759457465`
  - workflow: `dryrun-candidate-gate.yml`
  - git ref: `main@d43873a72cb9c8a51313b0f6a301c4dfe1bb1978`
  - input: `mode=candidate-quality`
  - artifact: `dryrun-candidate-gate-25759457465`
  - status: `blocked`
  - failures: closed trades `7 < 50`
  - values: realized PnL `15.33`, profit factor `1.3359`, max drawdown
    `-30.468`, buy fill rate `98.01%`
  - interpretation: the current post-reset dry-run sample is positive but
    under-sampled. Treat as `collect-more`, not a tradable strategy.
- dry-run candidate-quality gate run: `25766027782`
  - workflow: `dryrun-candidate-gate.yml`
  - git ref: `main@f26490541d82127db78512e6a186bd84117c5340`
  - input: `mode=candidate-quality`
  - artifact: `dryrun-candidate-gate-25766027782`
  - status: `blocked`
  - failures: closed trades `7 < 50`
  - values: realized PnL `15.33`, profit factor `1.3359`, max drawdown
    `-30.468`, buy fill rate `98.01%`
  - interpretation: no new closed trades were available at this recheck. The
    dry-run remains `collect-more`; do not promote.

Current dry-run profitability read:

- checked_at: `2026-05-13 02:20 +08` range
- deployment: `pm5d.threelayer.settlement-probability-btc-eth.dryrun`
- state: `desired_state=running`, `observed_state=running`,
  `deployment_state=enabled`
- closed trades: `20`
- wins / losses: `9 / 11`
- realized PnL: `-115.17`
- profit factor: `0.2927`
- max drawdown: `-136.3244`
- buy fill rate: `97.93%` notional fill basis, with `20` buy orders and
  `300.00` requested notional
- interpretation: current dry-run is rejected / not tradeable; do not promote.
  The deployment is still running, so destructive reset remains blocked until
  explicit operator approval to pause or stop this dry-run lane.

Current post-reset dry-run read:

- checked_at: `2026-05-13 05:08 +08`
- deployment: `pm5d.threelayer.settlement-probability-btc-eth.dryrun`
- state: `desired_state=running`, `observed_state=running`,
  `deployment_state=enabled`
- closed trades: `7`
- realized PnL: `15.33`
- profit factor: `1.3359`
- max drawdown: `-30.468`
- buy fill rate: `98.01%`
- interpretation: still not tradeable. Positive PnL over 7 closed trades is
  not enough to promote; the candidate-quality gate requires at least 50 closed
  trades before the PnL/profit-factor read can matter.

Historical parity runs that led to the ready handoff:

- run: `25687392088`
- workflow: `recorded-replay-parity.yml`
- status: `completed`
- result: runtime/event strict ready
- artifact: `recorded-replay-parity-25687392088`
- blocking flags: `[]`
- interpretation: this parity artifact was used by hosted walk-forward run
  `25687766026` and is sufficient for the config handoff decision.

Earlier blocker runs:

- run: `25683379408`
- workflow: `recorded-replay-parity.yml`
- status: `completed`
- result: workflow success, strict parity failed
- resolved auto window: `2026-05-11T13:46:06Z` ->
  `2026-05-11T16:56:00Z`
- reason: the auto window selected dry-run rows outside the available recording
  coverage, so replay produced no shared event/order/fill rows
- deploy impact: none; this run was a replay comparison, not a deploy

- run: `25685537480`
- workflow: `recorded-replay-parity.yml`
- status: `completed`
- result: workflow success, strict parity failed
- manual window: `2026-05-11T19:47:00+08:00` ->
  `2026-05-11T21:12:00+08:00`
- shared orders/fills: `18 / 18`
- blocking flags:
  `events_present_in_dryrun_missing_from_replay`,
  `orders_present_in_dryrun_missing_from_replay`,
  `runtime_evidence_field_mismatches`,
  `settlement_exit_price_mismatches`
- interpretation: the window mismatch is no longer the only issue. The current
  dry-run evidence still contains entry/settlement behavior that current replay
  cannot reproduce, so promotion remains correctly blocked.

## Prompt-To-Artifact Checklist

| Requirement | Evidence | Status | Notes |
| --- | --- | --- | --- |
| System can generate alpha candidates | `factor-walk-forward-v2-hosted-artifact.yml` emits alpha-search artifacts under `factor-walk-forward-v2/alpha-search/` | `ready` | Verified by runs `25683730944`, `25683858420`, `25683945041`, and `25684811262`. |
| System can explore with MCTS-style continuation | `mcts-expansion-plan.json`, `alpha_search_plan_run_id`, `chain_next_run`, and `alpha-search-chain/chain-decision.json` | `ready` | Run `25683730944` dispatched `25683858420`; run `25683858420` dispatched `25683945041`. |
| System records interpretable search artifacts | `search-space.json`, `llm-priors.json`, `candidate-expressions.json`, `rejected-expressions.json`, `tree-trace.json`, `node-metrics.json`, `search-feedback.json` | `ready` | These are uploaded in hosted artifacts. `llm-priors.json` is a typed placeholder prior, not free-form live LLM generation. |
| System summarizes search-chain evidence | `scripts/summarize_alpha_search_chain.py`, `alpha-search-chain/summary.json`, `alpha-search-chain/summary.md` | `ready` | PR #428 added the script. PR #429 wired it into the hosted workflow. Run `25684811262` proves the summary files are uploaded. |
| Search found a stronger candidate branch | run `25683858420` | `partial` | Best reward improved to `4.894975850946932` with `mcts_mcts_auto_settlement_conservative_settlement_edge_x_near_strike_near_strike_near_strike`, but the ready handoff selected the simpler conservative settlement edge. |
| Promotion gate blocks unsafe candidates | `autofactor-strategy-handoff.json` | `ready` | Earlier runs stayed blocked. Run `25687766026` became ready only after replay parity was supplied and gate blockers were empty. |
| Replay/dry-run parity is ready for handoff | `recorded-replay-parity.yml` artifact `recorded-replay-parity-25687392088` | `ready` | PR `#433` records `replay_parity_ready=true`, `runtime/event strict ready`, and `blocking=[]`. |
| A dry-run config PR can be generated from ready handoff | `create_config_pr=true` path in hosted workflow, PR `#433`, current `origin/main` config | `ready` | PR `#433` proved the config-PR path; the current dry-run config on `origin/main` uses `autofactor_formula:auto_settlement_conservative_settlement_edge_x_near_strike`. |
| Latest deploy bundle can be built without mutating remote services | `deploy-tango-1-1.yml` run `25766143754` with `deploy=false` | `ready` | Build-only run built the release runner, research tools, optimize-backtest, deploy bundle, and live-paused bundle guard from `main@f26490541d82127db78512e6a186bd84117c5340`. |
| Remote dry-run config matches the ready handoff on `main` | read-only `tango-1-1` config comparison plus current `origin/main` config check | `blocked` | Remote is still `auto_settlement_full_depth_settlement_edge`; `origin/main` is `auto_settlement_conservative_settlement_edge_x_near_strike`, so protected dry-run deploy is required before judging the current handoff. |
| Old dry-run runtime evidence can be cleared without touching raw data | PRs `#464`, `#465`, `#466`; reset preview `25748008417`; guard run `25748940434`; reset execute run `25754514193` | `done` | The guarded reset deleted only the scoped runtime rows: before `185` orders / `20` fills, after `0` / `0`. Raw market data was not part of the reset scope. |
| Reset procedure is documented and operator-gated | `docs/runbooks/strategy-runtime-evidence-reset.md`; PR `#470` | `ready` | Runbook records preflight, approval text, preview, pause, guarded execute, artifact inspection, resume, and post-reset gates. |
| Clean post-reset baseline can be machine-checked | `scripts/check_dryrun_candidate_gate.py`; PR `#471`; workflow `dryrun-candidate-gate.yml`; PR `#472`; reset-workflow post-gate PR `#474`; run `25754514193` | `passed` | The reset workflow and an independent current API check both passed with `target_strategy_absent`. |
| Dry-run candidate quality can be machine-checked | `scripts/check_dryrun_candidate_gate.py --mode candidate-quality`; run `25766027782`; current API check after reset | `ready and currently blocked` | Latest recheck still has only `7` closed trades, below the `50` trade minimum. Positive PnL over 7 trades is under-sampled and remains `collect-more`. |
| Entry-price-quality alpha prior is available to search and runtime handoff | PR `#484`, `tasks/alpha_search_priors/pm5d_entry_price_quality_prior_20260513.json`, runtime `autofactor_formula:*_x_entry_price_quality` suffixes | `ready` | This repairs a semantic/search gap for binary-ticket entry quality. It is search plumbing, not performance evidence. |
| Scheduled full retained-window audits enforce coverage | PR `#485`, `.github/workflows/market-data-gap-audit.yml` | `ready` | The `17 */6 * * *` UTC full schedule forces `gate_mode=coverage`; the 30-minute quick schedule remains freshness-only. Environment approval may still be required for protected `tango-1-1` access. |
| Current retained data window supports promotion-grade search | market-data audits `25747004738`, `25753059613`, `25758692951` | `blocked` | 24h coverage still includes the known Binance LOB gap for BTC/ETH through `2026-05-12 10:49 +08`. Wait for a clean retained window or use shorter diagnostic-only snapshots. |
| A profitable strategy has been produced | ready handoff plus post-reset dry-run/executable evidence | `not ready` | Old negative dry-run rows have been cleared. Fresh post-reset dry-run has `7` closed trades with positive PnL, but sample-size gate requires `50`; no profitability claim is allowed. |

## Latest Alpha-Search Evidence

All runs used:

- git ref: `main`
- snapshot: `25642459432`
- symbols: `BTCUSDT,ETHUSDT`
- window: `2026-04-24..2026-04-30`
- stake: `15`

| Run | Candidates | Passed | Best reward | Best selected factor | Handoff | Action |
| --- | ---: | ---: | ---: | --- | --- | --- |
| `25683730944` | 183 | 32 | 4.81954070569323 | `auto_settlement_conservative_settlement_edge` | blocked | do_not_promote |
| `25683858420` | 231 | 68 | 4.894975850946932 | `mcts_mcts_auto_settlement_conservative_settlement_edge_x_near_strike_near_strike_near_strike` | blocked | do_not_promote |
| `25683945041` | 203 | 47 | 4.81954070569323 | `auto_settlement_conservative_settlement_edge` | blocked | do_not_promote |
| `25684811262` | 183 | 32 | 4.81954070569323 | `auto_settlement_conservative_settlement_edge` | blocked | do_not_promote |
| `25687766026` | 183 | 32 | 4.81954070569323 | `auto_settlement_conservative_settlement_edge` | ready | create_dry_run_handoff |

## Landed System Improvements

- PR #426: `recorded-replay-parity.yml` supports `since=auto` and
  `until=auto`, writes `resolved-window.json`, and records the resolved window
  in artifacts/comments.
- PR #427: `docs/ALPHA_FACTOR_SEARCH_CICD.md` now states that alpha generation,
  MCTS planning, and hosted chaining exist; promotion evidence is the blocker.
- PR #428: `scripts/summarize_alpha_search_chain.py` summarizes downloaded
  hosted artifacts into chain-level JSON and Markdown.
- PR #429: hosted Factor Walk-Forward V2 now uploads
  `alpha-search-chain/summary.json` and `summary.md` for each run.
- PR #432: fixed alpha parity readiness gates so strict parity can be used by
  the hosted promotion chain.
- PR #433: promoted ready hosted AutoFactor handoff from run `25687766026` into
  `config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml`.
- PR #434: fixed hosted replay parity artifact option parsing so
  `<run-id>:<artifact-name>` inputs route correctly.
- PR #464: added backup-first `reset_strategy_runtime_evidence.py` and
  `reset-strategy-runtime-evidence.yml` for deployment-scoped dry-run runtime
  evidence cleanup.
- PR #465: added a fail-closed guard so `execute=true` refuses to run while the
  target deployment is desired/observed `running`, unless `allow_running=true`
  is explicitly supplied.
- PR #466: made running-guard failures upload `guard-status.json` so blocked
  destructive reset attempts have artifact evidence.
- PR #470: added `docs/runbooks/strategy-runtime-evidence-reset.md`, the
  operator runbook for preview, approval, pause, guarded reset, artifact
  inspection, and resume.
- PR #471: added `scripts/check_dryrun_candidate_gate.py` and focused tests so
  clean-baseline and candidate-quality decisions are machine-checked from the
  dry-run API payload.
- PR #472: added `dryrun-candidate-gate.yml`, a GitHub Actions surface that
  uploads dry-run report and gate-result artifacts for reset/promotion review.
- Run `25751440786`: proved the dry-run candidate gate blocks the current
  settlement-probability dry-run with `residual_runtime_evidence`.
- PR #474: wired the clean-baseline gate into
  `reset-strategy-runtime-evidence.yml` after `execute=true`, uploading
  `post-reset-dryrun-report.json` and
  `post-reset-clean-baseline-gate.json` in the reset artifact.

## Remaining Actions

1. Do not reset runtime evidence again unless a new contaminated window is
   identified. The scoped reset already completed in run `25754514193`, passed
   the post-reset clean-baseline gate, and resumed the dry-run deployment.
2. Keep
   `pm5d.threelayer.settlement-probability-btc-eth.dryrun` running in dry-run
   mode so post-reset closed trades can accumulate. The candidate-quality gate
   remains `collect-more` until at least `50` closed trades exist.
3. After `2026-05-13 10:49 +08`, rerun the 24h BTC/ETH market-data coverage
   audit with `gate_mode=coverage`. If it remains `critical`, classify the next
   decision as `fix-data` or `collect-more`, not promotion.
4. If the 24h audit is clean, compile a new full research snapshot from the
   clean exact timestamp window with `research-snapshot.yml`, `data_gate=critical`,
   `data_profile=pm5d-execution`, and `upload_full_snapshot=true`.
5. Run `factor-walk-forward-v2-hosted-artifact.yml` from `main` using that
   snapshot artifact and
   `tasks/alpha_search_priors/pm5d_entry_price_quality_prior_20260513.json`,
   with `required_strategy_profile=settlement_probability` and
   `allowed_target=full_depth_settlement_executable_pnl`.
6. Do not create a config PR from the new search unless
   `autofactor-strategy-handoff.json` is `ready`, the data audit is clean, and
   the parity plan is explicit.
7. Rerun recorded replay parity against the fresh clean dry-run sample before
   promotion or live discussion.
8. Review fresh dry-run PnL, fills, drawdown, capacity, and settlement-exit
   evidence before calling any strategy profitable.

## Completion Rule

Do not mark the user objective complete until all of the following are true:

- alpha-search artifact bundle exists for the selected candidate;
- `autofactor-strategy-handoff.json` reports `status=ready`;
- recorded replay/dry-run parity reports strict readiness for both runtime and
  event evidence;
- old runtime rows are reset or the report is explicitly filtered to a clean
  post-reset observation window;
- reset artifact `post-reset-clean-baseline-gate.json` has `status=passed`, or
  a manual `check_dryrun_candidate_gate.py --mode clean-baseline` /
  `dryrun-candidate-gate.yml` recheck passes for the selected dry-run
  deployment;
- the retained data window used for promotion is clean enough for the declared
  evidence stage;
- the dry-run config PR is generated from the ready handoff and passes CI;
- executable dry-run evidence supports profitability after costs, fills,
  settlement, and drawdown constraints.
