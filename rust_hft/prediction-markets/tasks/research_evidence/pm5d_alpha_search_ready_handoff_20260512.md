# Research Evidence: PM5D Alpha Search Ready Handoff - 2026-05-12

## Decision

Status: `promote-to-dry-run`

One-line decision: Run `25737965398` produced a ready dry-run handoff for the
PM5D settlement-probability lane, but this is not live-promotion evidence.

## Semantic Context

- Strategy lane: `settlement_probability`
- Evidence stage: `walk_forward`
- Lifecycle segment tested: `signal -> pnl`
- Promotion target, if any: `dry-run`

## Hypothesis

- Claim: Conservative settlement edge, optionally gated by near-strike event
  geometry, can rank PM5D BTC/ETH binary-option entries with positive
  executable settlement PnL after full-depth entry pricing.
- Expected edge mechanism: Buy the side whose estimated settlement probability
  exceeds executable entry cost, while requiring the full-depth settlement
  target and runtime replay parity gate.
- Failure criteria: No qualified strategy, blocked promotion gate, missing
  replay parity, unstable symbol/window behavior, or missing runtime scorer
  mapping.
- Next decision this evidence should unlock: create or keep a dry-run handoff
  and collect a fresh post-reset dry-run window before any live discussion.

## Inputs

- Git ref: `main@025e90b657ee641bcbb35e1411ccde3bf5c74c42`
- Workflow run: `25737965398`
- Snapshot or artifact: `research-snapshot-25642459432`,
  `factor-walk-forward-v2-25737965398`
- Dataset/window: `2026-04-24 00:00:00 UTC` ->
  `2026-05-01 00:00:00 UTC`
- Symbols/events: `BTCUSDT,ETHUSDT`; event-complete gate reported
  `event_complete_events=292`, `event_complete_rows=468`
- Config: `config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml`
- Local/remote artifact paths:
  `/tmp/pm5d-alpha-25737965398/factor-walk-forward-v2-25737965398`

## Data Surface Audit

- Binance spot/trade ticks: `present`
- Binance L2/LOB: `present`
- Polymarket quote ticks: `present`
- Polymarket full CLOB depth: `present`
- Official settlement: `present`
- Runtime/dry-run fills: `present for parity gate`
- Data audit status: `critical` on the broad snapshot audit; PM5D scoped
  event-complete gate passed for this run.
- Missing surfaces and impact: Broad snapshot audit caveats remain a reason to
  keep this at dry-run handoff, not live promotion.

## Accounting Semantics

- Event accounting: `one-event-one-trade`
- Entry price model: full-depth executable entry sweep for fixed `15` USD stake
- Exit or settlement label: official settlement / full-depth settlement
  executable PnL
- Fillability assumption: promotion gate used `min_entry_fill_rate=0.0500`
  with event-complete rows
- Fees/slippage/latency assumption: workflow report used executable full-depth
  pricing and quote-age constraints
- Capacity or stake assumption: `stake_usd=15`

## Results

- Headline metrics: `237` generated candidates, `71` passed search feedback,
  best search reward `4.894975850946932`.
- Stability metrics: handoff strategies both had `positive_window_ratio=1.0`,
  `symbol_positive_ratio=1.0`, and `window_count=4`.
- Calibration or bucket behavior: promotion gate evidence required
  `max_ece=0.0500`.
- Fill rate/capacity: promotion gate evidence required
  `min_entry_fill_rate=0.0500`.
- PnL/ROI:
  - `auto_settlement_conservative_settlement_edge`: `n=377`, `icir=2.0567`,
    `spearman_ic=0.160886`, `top_bucket_avg_label=2.154376`,
    `top_bucket_positive_label_rate=0.6053`.
  - `auto_settlement_conservative_settlement_edge_x_near_strike`: `n=377`,
    `icir=1.999983`, `spearman_ic=0.163584`,
    `top_bucket_avg_label=2.825599`,
    `top_bucket_positive_label_rate=0.6316`.
- Drawdown/risk: Not established by this evidence; must be observed in a fresh
  dry-run window with kill switch and fixed stake.

## Promotion Gate Check

- Hypothesis explicit: `pass`
- Data provenance recorded: `pass`
- Executable pricing conservative: `pass`
- Settlement/exit label matches lane: `pass`
- Walk-forward or leakage guard: `pass`
- Replay/runtime parity: `pass` via recorded replay parity run `25737603431`
  with `strict_parity_ready=true`
- Runtime scorer/config mapping: `pass` for the two handoff strategies
- Risk/stake/kill switch stated: `pass for stake`; kill-switch verification is
  required before deployment monitoring

## Caveats

- This is a dry-run handoff only. It is not evidence to trade live.
- The current dry-run report still contains older rows, so fresh strategy
  performance must be measured from a reset or explicitly bounded post-cutover
  window.
- The prior dry-run config already used
  `autofactor_formula:auto_settlement_conservative_settlement_edge`; this
  branch applies the near-strike variant for the next dry-run comparison.

## Follow-Up

- Open and merge a dry-run handoff/config PR for the near-strike variant.
- Reset or window the dry-run evidence so old rows cannot contaminate the next
  profitability read.
- Deploy dry-run from `main`, then collect fresh quote quality, fills,
  settlement PnL, drawdown, and replay parity before any live-candidate step.
