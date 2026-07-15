# Research Evidence: PM5D Settlement Probability Dry-Run Diagnostic - 2026-05-12

## Decision

Status: `revise`

One-line decision: Runtime same-event side mixing is fixed in the fresh dry-run
sample, but the strategy is not live-usable because recorded replay parity is
blocked and quote freshness has intermittent Polymarket WebSocket resets.

## Semantic Context

- Strategy lane: `settlement_probability`
- Evidence stage: `dry_run_candidate`
- Lifecycle segment tested: `order | fill | position | pnl | runtime_parity | execution_quality`
- Promotion target, if any: `none`

## Hypothesis

- Claim: The deployed settlement-probability AutoFactor profile can trade PM5D
  BTC/ETH binary events with one event, one decision, one trade lifecycle.
- Expected edge mechanism: Buy the side whose estimated settlement probability
  exceeds executable entry cost after freshness and edge gates.
- Failure criteria: same-event UP/DOWN entries, stale or missing executable PM
  quotes, replay/dry-run mismatch, missing official settlement accounting, or
  underpowered dry-run sample.
- Next decision this evidence should unlock: Continue dry-run observation only
  after fixing replay/runtime parity; do not promote to live.

## Inputs

- Git ref: `origin/main@b0591b57`
- Workflow run: `recorded-replay-parity` run `25714741551`
- Snapshot or artifact: `recorded-replay-parity-25714741551`
- Dataset/window: fresh dry-run window from `2026-05-12 12:26:19 +08`; parity
  auto-window `2026-05-12T04:26:19Z -> 2026-05-12T04:46:13Z`
- Symbols/events: BTCUSDT/ETHUSDT PM5D events
- Config: `/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml`
- Local/remote artifact paths:
  - `/tmp/pm5d-parity-25714741551/parity-evaluation.json`
  - `/tmp/pm5d-parity-inputs-25714741551/replay/evaluation.json`
  - `strategy_runtime_orders_backup_20260512_same_event_guard`
  - `strategy_runtime_fills_backup_20260512_same_event_guard`

## Data Surface Audit

- Binance spot/trade ticks: `present`
- Binance L2/LOB: `present`
- Polymarket quote ticks: `present`
- Polymarket full CLOB depth: `partial`
- Official settlement: `present`
- Runtime/dry-run fills: `present`
- Data audit status: `caveated`
- Missing surfaces and impact: Active PM quote coverage was mostly present, but
  `8 / 20` active BTC/ETH tokens had quote age greater than 15 seconds and
  `1 / 20` lacked ask/ask_size at the check time. Runtime should skip stale
  quotes via `three_layer_max_pm_lag_secs=15`, but opportunity coverage is
  degraded.

## Accounting Semantics

- Event accounting: `one-event-one-trade`
- Entry price model: dry-run BUY limit at recorded top ask with fixed
  `stake_usd=15`
- Exit or settlement label: official settlement residual in
  `strategy_runtime_event_track_record`
- Fillability assumption: observed dry-run fills; active quote ask size must
  cover fixed stake
- Fees/slippage/latency assumption: dry-run recorded fees/slippage; no live
  queue-position proof
- Capacity or stake assumption: `stake_usd=15`, `max_positions=4`,
  `max_daily_trades=50`

## Results

- Headline metrics: Fresh cleaned dry-run sample had `4` orders, `4` fills, `4`
  confirmed closed track-record rows.
- Stability metrics: Too few events for stability; sample is underpowered.
- Calibration or bucket behavior: Not evaluated in this diagnostic.
- Fill rate/capacity: `100%` BUY fill rate, `$60.00` requested and filled
  notional, `0` rejected or partial BUY orders.
- PnL/ROI: Reported realized PnL `+18.76`, total fees `0.27`, win rate `100%`
  over `4` trades. This is not promotion evidence because replay parity failed.
- Drawdown/risk: Reported max drawdown `0.0`, open exposure `0.0` at the check
  time; sample is too small.

## Promotion Gate Check

- Hypothesis explicit: `pass`
- Data provenance recorded: `pass`
- Executable pricing conservative: `caveated`
- Settlement/exit label matches lane: `caveated`
- Walk-forward or leakage guard: `fail`
- Replay/runtime parity: `fail`
- Runtime scorer/config mapping: `pass`
- Risk/stake/kill switch stated: `pass`

## Caveats

- Recorded replay parity run `25714741551` returned
  `decision=fix-data-or-runtime-mismatch` with blocking flags:
  `events_present_in_replay_missing_from_dryrun`,
  `orders_present_in_replay_missing_from_dryrun`, and
  `fills_present_in_replay_missing_from_dryrun`.
- Replay produced settlement SELL rows and additional BUY entries that the
  dry-run did not record. This means the report view can close rows via official
  residual settlement while the replay/runtime lifecycle is not strictly
  equivalent.
- Polymarket quote collector had repeated WebSocket
  `ResetWithoutClosingHandshake` errors in the prior 30 minutes. Code change in
  this branch now rebuilds the subscription after stream errors, refreshes
  unchanged cached top-of-book rows every 5 seconds, and preserves valid near-
  terminal binary-option prices such as `0.99` / `0.01`.
- Legacy DB backtest workflow run `25714741515` targeted the old `ploy-ci-1`
  self-hosted path and did not produce usable evidence. Current follow-up should
  use GitHub-hosted artifact workflows when a retained snapshot/artifact is
  available; `ploy-ci-1` is only a DB-adjacent export fallback, not the default
  research runner.

## Follow-Up

- Fix recorded replay parity so settlement exits and runtime dry-run accounting
  use the same lifecycle semantics.
- Decide whether dry-run should persist synthetic settlement SELL fills or
  parity should compare official residual settlement rows without requiring
  synthetic order/fill parity.
- Deploy the quote collector freshness fix from `main`, then re-check active
  BTC/ETH token quote age against `three_layer_max_pm_lag_secs=15`.
- Re-run parity/backtest evidence through the GitHub-hosted artifact path when
  a suitable retained snapshot exists; only use `ploy-ci-1` for fresh DB export
  work that cannot yet run from a portable artifact.
