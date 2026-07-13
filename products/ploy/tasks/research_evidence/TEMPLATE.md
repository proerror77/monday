# Research Evidence: <topic> - <YYYY-MM-DD>

## Decision

Status: `<continue | revise | reject | promote-to-dry-run | promote-to-live>`

One-line decision:

## Semantic Context

- Strategy lane: `<settlement_probability | repricing | runtime_parity | execution_quality | other>`
- Evidence stage: `<diagnostic | factor_attribution | executable_replay | walk_forward | runtime_parity | dry_run_candidate | live_candidate>`
- Lifecycle segment tested: `<signal | intent | order | fill | position | pnl | risk | end-to-end>`
- Promotion target, if any: `<none | dry-run | live>`

## Hypothesis

- Claim:
- Expected edge mechanism:
- Failure criteria:
- Next decision this evidence should unlock:

## Inputs

- Git ref:
- Workflow run:
- Snapshot or artifact:
- Dataset/window:
- Symbols/events:
- Config:
- Local/remote artifact paths:

## Data Surface Audit

- Binance spot/trade ticks: `<present | missing | not applicable>`
- Binance L2/LOB: `<present | missing | not applicable>`
- Polymarket quote ticks: `<present | missing | not applicable>`
- Polymarket full CLOB depth: `<present | missing | not applicable>`
- Official settlement: `<present | missing | not applicable>`
- Runtime/dry-run fills: `<present | missing | not applicable>`
- Data audit status:
- Missing surfaces and impact:

## Accounting Semantics

- Event accounting: `<one-event-one-trade | multi-entry-defined | diagnostic-entry-grid>`
- Entry price model:
- Exit or settlement label:
- Fillability assumption:
- Fees/slippage/latency assumption:
- Capacity or stake assumption:

## Results

- Headline metrics:
- Stability metrics:
- Calibration or bucket behavior:
- Fill rate/capacity:
- PnL/ROI:
- Drawdown/risk:

## Promotion Gate Check

- Hypothesis explicit: `<pass | fail | n/a>`
- Data provenance recorded: `<pass | fail | n/a>`
- Executable pricing conservative: `<pass | fail | n/a>`
- Settlement/exit label matches lane: `<pass | fail | n/a>`
- Walk-forward or leakage guard: `<pass | fail | n/a>`
- Replay/runtime parity: `<pass | fail | n/a>`
- Runtime scorer/config mapping: `<pass | fail | n/a>`
- Risk/stake/kill switch stated: `<pass | fail | n/a>`

## Caveats

-

## Follow-Up

-
