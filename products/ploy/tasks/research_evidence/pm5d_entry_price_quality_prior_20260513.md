# Research Evidence: PM5D Entry-Price Quality Alpha Prior - 2026-05-13

## Decision

Status: `continue`

One-line decision: add entry-price quality as a bounded research/runtime feature
so alpha search can penalize brittle binary-ticket prices before any dry-run
handoff.

## Semantic Context

- Strategy lane: `settlement_probability`
- Evidence stage: `factor_attribution`
- Lifecycle segment tested: `signal`
- Promotion target, if any: `none`

## Hypothesis

- Claim: settlement edge should be gated by executable entry-price quality,
  because very low-priced or very expensive binary tickets are more likely to
  turn apparent probability edge into fragile execution or calibration edge.
- Expected edge mechanism: retain settlement-probability candidates only when
  the entry price is in a range where the 15U stake has usable payout structure
  and less brittle quote behavior.
- Failure criteria: walk-forward/MCTS rejects the entry-price-quality variants,
  symbol holdout weakens, or dry-run/replay parity cannot map the selected
  formula into the runtime scorer.
- Next decision this evidence should unlock: run hosted artifact alpha search
  after a clean retained BTC/ETH data window is available.

## Inputs

- Git ref: pending branch `research/pm5d-low-price-alpha-prior`
- Workflow run: not run yet
- Snapshot or artifact: none
- Dataset/window: none
- Symbols/events: intended BTCUSDT, ETHUSDT PM5D
- Config:
  `tasks/alpha_search_priors/pm5d_entry_price_quality_prior_20260513.json`
- Local/remote artifact paths: local repo files only

## Data Surface Audit

- Binance spot/trade ticks: `not applicable`
- Binance L2/LOB: `not applicable`
- Polymarket quote ticks: `not applicable`
- Polymarket full CLOB depth: `not applicable`
- Official settlement: `not applicable`
- Runtime/dry-run fills: `not applicable`
- Data audit status: not run; known 24h BTC/ETH LOB gap must age out before
  promotion-grade walk-forward.
- Missing surfaces and impact: this is prior/schema work only, not performance
  evidence.

## Accounting Semantics

- Event accounting: `one-event-one-trade`
- Entry price model: bounded `entry_price_quality_score` from executable
  `entry_ask`
- Exit or settlement label: future run must use
  `full_depth_settlement_executable_pnl`
- Fillability assumption: future run must use full-depth / conservative
  executable pricing and capacity gates
- Fees/slippage/latency assumption: future run must retain spread and quote-age
  gates
- Capacity or stake assumption: future run should keep 15U stake semantics

## Results

- Headline metrics: none; no backtest was run.
- Stability metrics: none.
- Calibration or bucket behavior: to be evaluated by hosted walk-forward.
- Fill rate/capacity: not tested.
- PnL/ROI: not tested.
- Drawdown/risk: not tested.

## Promotion Gate Check

- Hypothesis explicit: `pass`
- Data provenance recorded: `n/a`
- Executable pricing conservative: `n/a`
- Settlement/exit label matches lane: `n/a`
- Walk-forward or leakage guard: `n/a`
- Replay/runtime parity: `n/a`
- Runtime scorer/config mapping: `pass`
- Risk/stake/kill switch stated: `n/a`

## Caveats

- This is not evidence that a strategy is profitable.
- The new feature is intentionally bounded and conservative; it can reduce
  search reward by rejecting low-priced or expensive tickets that previously
  looked attractive on raw edge.
- Promotion remains blocked until clean data audit, hosted walk-forward,
  ready handoff, fresh dry-run sample, and recorded replay/dry-run parity pass.

## Follow-Up

- After `2026-05-13 10:48 +08`, rerun the BTC/ETH 24h market-data audit.
- If clean, run hosted artifact alpha search with
  `alpha_search_llm_prior_json=tasks/alpha_search_priors/pm5d_entry_price_quality_prior_20260513.json`.
