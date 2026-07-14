# Research Evidence: PM5D Hosted Alpha Search Continuation - 2026-05-12

## Decision

Status: `revise`

One-line decision: Hosted alpha search found and extended candidate settlement
factors, but no candidate is promotable because the latest recorded replay
parity artifact blocks dry-run handoff.

## Semantic Context

- Strategy lane: `settlement_probability`
- Evidence stage: `factor_attribution | walk_forward`
- Lifecycle segment tested: `signal | pnl`
- Promotion target, if any: `none`

## Hypothesis

- Claim: Prior PM5D settlement/liquidity factors can be reused as typed
  alpha-search priors, then expanded by MCTS-guided mutations to find stronger
  settlement-probability factors for BTC/ETH five-minute Polymarket events.
- Expected edge mechanism: Estimate settlement probability and only prefer a
  side when the settlement edge survives executable entry price, full-depth
  fillability, spread/capacity penalties, and event geometry gates.
- Failure criteria: No positive walk-forward candidates, reward stagnation,
  missing search artifacts, blocked promotion gate, replay/runtime mismatch, or
  candidate complexity increasing faster than robustness.
- Next decision this evidence should unlock: Fix replay/runtime parity before
  any dry-run promotion; keep `auto_settlement_conservative_settlement_edge`
  and the near-strike MCTS variants as research candidates only.

## Inputs

- Git ref: `main@d4f9ff48`
- Workflow runs:
  - `25722101811`: failed pre-research because `end_date=2026-05-01` resolved
    outside the snapshot window.
  - `25722193283`: successful hosted alpha-search continuation.
  - `25722317462`: successful chained alpha-search follow-up.
- Snapshot or artifact:
  - `research-snapshot-25642459432`
  - `recorded-replay-parity-25714741551`
  - prior MCTS artifact `factor-walk-forward-v2-25707061616`
- Dataset/window: `2026-04-24T00:00:00Z -> 2026-05-01T00:00:00Z`
- Symbols/events: `BTCUSDT,ETHUSDT` PM5D events
- Config: `config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml`
- Local/remote artifact paths:
  - `/tmp/ploy-alpha-25722193283`
  - `/tmp/ploy-alpha-25722317462`
  - GitHub Actions artifact `factor-walk-forward-v2-25722193283`
  - GitHub Actions artifact `factor-walk-forward-v2-25722317462`

## Data Surface Audit

- Binance spot/trade ticks: `present`
- Binance L2/LOB: `present`
- Polymarket quote ticks: `present`
- Polymarket full CLOB depth: `present`
- Official settlement: `present`
- Runtime/dry-run fills: `present for parity artifact, but mismatched`
- Data audit status: `caveated`
- Missing surfaces and impact: No missing research snapshot surface blocked the
  walk-forward runs. Promotion is blocked because the latest replay parity
  artifact reports runtime/dry-run lifecycle mismatch.

## Accounting Semantics

- Event accounting: `one-event-one-trade`
- Entry price model: fixed `stake_usd=15` with executable full-depth settlement
  target `full_depth_settlement_executable_pnl`
- Exit or settlement label: official settlement target in the retained research
  snapshot
- Fillability assumption: full-depth executable settlement PnL target with
  event-complete data quality
- Fees/slippage/latency assumption: encoded in the factor walk-forward target
  and promotion evaluator; no live queue-position proof
- Capacity or stake assumption: `stake_usd=15`

## Results

- Headline metrics:
  - Run `25722193283`: `237` candidates, `71` passed, best factor
    `mcts_mcts_auto_settlement_conservative_settlement_edge_x_near_strike_near_strike_near_strike`,
    best reward `4.894975850946932`, handoff `blocked`.
  - Run `25722317462`: `193` candidates, `38` passed, best factor
    `auto_settlement_conservative_settlement_edge`, best reward
    `4.81954070569323`, handoff `blocked`.
- Stability metrics: The best factors report positive-window ratio `1.0` on
  this retained walk-forward window, but this is still one artifact window and
  must not be treated as production stability.
- Calibration or bucket behavior: Best first-run candidate had top-bucket
  average label `2.6722902978466125`; the simpler conservative settlement edge
  had top-bucket average label `2.1543761988825487`.
- Fill rate/capacity: Evaluated through the full-depth settlement executable
  PnL target; runtime/dry-run fill parity remains blocked.
- PnL/ROI: Reward and top-bucket labels are discovery metrics, not executable
  strategy PnL.
- Drawdown/risk: Not sufficient for promotion in this evidence stage.

## Promotion Gate Check

- Hypothesis explicit: `pass`
- Data provenance recorded: `pass`
- Executable pricing conservative: `pass`
- Settlement/exit label matches lane: `pass`
- Walk-forward or leakage guard: `pass`
- Replay/runtime parity: `fail`
- Runtime scorer/config mapping: `caveated`
- Risk/stake/kill switch stated: `caveated`

## Caveats

- Both successful runs were blocked by:
  `recorded_replay_parity: runtime_ready=false event_ready=true
  blocking_flags=events_present_in_replay_missing_from_dryrun|orders_present_in_replay_missing_from_dryrun|fills_present_in_replay_missing_from_dryrun
  decision=fix-data-or-runtime-mismatch`.
- The first successful run improved best reward from the prior `4.81954070569323`
  to `4.894975850946932`, but the best factor is a high-complexity repeated
  near-strike MCTS mutation with lower runtime readiness than the simple
  conservative settlement edge.
- The chained follow-up stopped with `reward_stagnation`: current best reward
  `4.81954070569323` was below previous best `4.894975850946932`.
- Search feedback explicitly states that alpha-search evidence is discovery
  evidence only; promotion still requires AutoFactor promotion gate and
  replay/runtime parity.

## Follow-Up

- Fix recorded replay parity for the settlement-probability dry-run lane before
  promoting any candidate.
- After parity is ready, rerun hosted alpha search with the same retained
  snapshot plus a fresh parity artifact, then require `autofactor-strategy-handoff.json`
  status `ready`.
- Prefer the simpler `auto_settlement_conservative_settlement_edge` for runtime
  handoff unless repeated near-strike variants prove incremental value across
  additional windows without excessive complexity.
