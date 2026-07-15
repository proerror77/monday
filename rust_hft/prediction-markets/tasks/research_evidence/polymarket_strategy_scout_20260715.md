# Research Evidence: Polymarket BTC/SOL 5m strategy scout - 2026-07-15

## Decision

Status: `continue`

One-line decision: Keep the existing settlement-probability stack; test CEX-to-Polymarket stale-quote markout and time-to-resolution depth decay as separate execution-microstructure hypotheses before adding any new factor.

## Semantic Context

- Strategy lane: `other`
- Evidence stage: `diagnostic`
- Lifecycle segment tested: `signal`
- Promotion target, if any: `none`

## Hypothesis

- Claim: Public evidence supports a Chainlink-native digital probability baseline and fee-aware CLOB evaluation; two additional microstructure hypotheses merit point-in-time tests.
- Expected edge mechanism: Settlement-source probability error, stale passive quotes after CEX repricing, or deteriorating depth near resolution.
- Failure criteria: No out-of-sample calibration improvement or no positive conservative markout/PnL after fees, spread, slippage, missed fills, and capacity.
- Next decision this evidence should unlock: Select the first execution-microstructure mission without expanding the settlement-factor framework.

## Inputs

- Git ref: `e46fce5b`
- Workflow run: Local Grok Build CLI `0.2.72`, model `grok-4.5`, session `019f64a4-8658-79e3-9a29-03825d321b07`
- Snapshot or artifact: Structured Grok synthesis from the source packet listed below; no market-data snapshot was evaluated.
- Dataset/window: Public dataset metadata reports about 89,000 markets and 26.8M per-second observations from March-May 2026; data not imported in this scout.
- Symbols/events: BTC and SOL recurring 5-minute Up/Down markets.
- Config: Research-only, no local tools, no order or deployment authority.
- Local/remote artifact paths: This file; Grok session remains in the user-local Grok session store.

## Data Surface Audit

- Binance spot/trade ticks: `missing`
- Binance L2/LOB: `missing`
- Polymarket quote ticks: `missing`
- Polymarket full CLOB depth: `missing`
- Official settlement: `missing`
- Runtime/dry-run fills: `missing`
- Data audit status: Source discovery only; no local data was used to claim an edge.
- Missing surfaces and impact: Every candidate remains a hypothesis until a content-addressed, arrival-timestamped BTC/SOL snapshot is evaluated.

## Source Packet

- BTC rule and Chainlink settlement source: https://polymarket.com/event/btc-updown-5m-1772577000
- SOL rule and Chainlink settlement source: https://polymarket.com/event/sol-updown-5m-1771984500
- Polymarket RTDS Binance and Chainlink feeds: https://docs.polymarket.com/market-data/websocket/rtds
- Polymarket CLOB order-book events: https://docs.polymarket.com/trading/orderbook
- Polymarket fee formula and per-market fee parameters: https://docs.polymarket.com/trading/fees
- Public BTC/SOL 5m top-of-book dataset: https://huggingface.co/datasets/kachoio/polymarket-5-minute-crypto-up-down-markets
- Polymarket microstructure preprint: https://arxiv.org/abs/2604.24366
- Community Binance-to-Polymarket lead-lag study, weak evidence: https://www.reddit.com/r/Polymarket/comments/1udy8xe/measuring_binancetopolymarket_leadlag_on_5minute/
- Community multi-strategy study, weak evidence: https://www.reddit.com/r/Polymarket/comments/1un85mg/i_spent_7_months_testing_every_strategy_on/
- General prediction-market calibration study: https://onlinelibrary.wiley.com/doi/10.1111/j.1468-0297.2012.02561.x

## Candidate Ranking

| Rank | Candidate | Lane | Current fit | Evidence | First test |
|---:|---|---|---|---|---|
| 1 | Chainlink digital fair versus CLOB mid | settlement probability | reuse `chainlink_digital` | strong | OOS Brier/logloss/ECE and settlement PnL |
| 2 | CLOB midpoint calibration baseline | settlement probability | reuse `market_midpoint` | medium | Reliability curves by symbol and time remaining |
| 3 | Fee, half-spread, slippage, and capacity hurdle | execution quality | reuse current evaluator | strong | Reject every candidate that only works at midpoint or zero fees |
| 4 | Distance, LOB, volatility residual beyond midpoint | settlement probability | reuse `distance_lob_vol` | medium | Nested OOS comparison after midpoint and Chainlink digital |
| 5 | Binance lead and stale passive-quote markout | repricing/execution | minimal extension | weak | Lagged Binance fair versus 1-30s Polymarket markout with conservative fills |
| 6 | Depth decay as time-to-resolution approaches zero | execution quality | reuse time buckets; extend capacity evidence | medium | Full-depth cost and fillability by time-remaining bucket |
| 7 | Passive maker versus directional taker economics | execution quality | data gap | weak | Queue-aware conservative maker simulation versus taker baseline |
| 8 | BTC/SOL cross-coin digital residual | relative value | defer | weak | Test only after single-symbol residuals survive all gates |

## Accounting Semantics

- Event accounting: `one-event-one-trade`
- Entry price model: Side-specific executable CLOB ask for taker tests; conservative queue/fill model for maker tests.
- Exit or settlement label: Chainlink open/close for settlement; future executable bid or markout for repricing.
- Fillability assumption: Full-depth walk with missed-fill and queue uncertainty; never midpoint fills.
- Fees/slippage/latency assumption: Query per-market fee parameters and charge fee, spread, depth, and arrival-time latency.
- Capacity or stake assumption: Fixed small stake first, capped by point-in-time displayed depth.

## Results

- Headline metrics: No performance metrics; this is a strategy-source scout.
- Stability metrics: Not evaluated.
- Calibration or bucket behavior: Not evaluated.
- Fill rate/capacity: Not evaluated.
- PnL/ROI: Not evaluated.
- Drawdown/risk: Not evaluated.

## Promotion Gate Check

- Hypothesis explicit: `pass`
- Data provenance recorded: `pass`
- Executable pricing conservative: `n/a`
- Settlement/exit label matches lane: `pass`
- Walk-forward or leakage guard: `n/a`
- Replay/runtime parity: `n/a`
- Runtime scorer/config mapping: `n/a`
- Risk/stake/kill switch stated: `n/a`

## Rejected Claims

- Treating Binance or another CEX as the settlement oracle.
- Inferring signed order flow solely from public CLOB prints; the cited preprint reports only about 59% agreement with on-chain trade direction.
- Unconditional trend, favorite, or longshot taker strategies without a point-in-time fee-aware replication.
- Any result based on midpoint fills, zero fees, infinite best-level depth, future volatility, or post-resolution data.
- Calling a future-mid repricing signal a settlement edge.
- Adding a cross-coin framework before single-symbol residuals demonstrate out-of-sample value.

## Caveats

- The local Grok CLI's native WebSearch/WebFetch tools were not exposed in this build, and its `grok-build` tool profile hit an existing terminal-tool parameter conflict. Sources were fetched independently and supplied to Grok as a bounded packet for structured synthesis.
- Reddit results are discovery evidence only and must not be promoted without local replication.
- Dataset metadata does not prove timestamp, settlement, full-depth, fee, or fill semantics match the PLOY contracts.

## Follow-Up

- Build one content-addressed BTC/SOL snapshot containing synchronized arrival timestamps for Chainlink, Binance, Polymarket full depth, and official settlement.
- Run the existing probability stack first; then open a separate repricing/execution mission for stale-quote markout and depth-decay capacity.
- Do not add the cross-coin residual factor unless the single-symbol residual tests pass.
