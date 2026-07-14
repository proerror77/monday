# Research Evidence: PM5D settlement-probability dry-run revision - 2026-05-27

## Decision

Status: `revise`

One-line decision: The dry-run runtime and fill path are healthy, but the
current settlement-probability AutoFactor score is losing money after costs and
must be revised before any live-candidate discussion.

## Semantic Context

- Strategy lane: `settlement_probability`
- Evidence stage: `dry_run_candidate`
- Lifecycle segment tested: `signal -> intent -> order -> fill -> position -> pnl`
- Promotion target, if any: `none`

## Hypothesis

- Claim: `autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted`
  should produce positive small-stake dry-run PnL when run with full-depth
  executable entry pricing and official settlement exits.
- Expected edge mechanism: settlement probability should exceed executable
  entry cost after capacity and spread adjustment.
- Failure criteria: negative net PnL, profit factor below `1.0`, unstable
  symbol/side buckets, or execution evidence showing rejected or unfillable
  entries.
- Next decision this evidence should unlock: feed runtime dry-run feedback back
  into Research Manager / AutoFactor typed priors and search for a revised
  runtime candidate; do not promote or scale this candidate.

## Inputs

- Git ref: `main@d4718d9e5a40af1233f83ec3a5b7e4560b2527a9`
- Workflow run: dry-run deploy evidence from deploy run `26498545236`; live
  remote verification performed on `2026-05-27 21:39-21:41 CST`.
- Snapshot or artifact: deployed Tango dry-run runtime evidence and public
  dry-run report.
- Dataset/window: target deployment rows from `2026-05-27 09:27:02 +08` through
  `2026-05-27 21:41:42 +08`; clean post-reset review window starts at
  `2026-05-27 16:08:40 +08`.
- Symbols/events: `BTCUSDT`, `ETHUSDT`; runtime event-track records for
  `pm5d.threelayer.settlement-probability-btc-eth.dryrun`.
- Config: `config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml`
  with `stake_usd=15`, `max_positions=4`, and `max_daily_trades=0`
  (unlimited dry-run evidence collection).
- Local/remote artifact paths:
  - Public report: `http://8.221.143.151/api/reports/dry-run`
  - Deployment state: `http://8.221.143.151/api/deployments`
  - Runtime recording:
    `/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson`

## Data Surface Audit

- Binance spot/trade ticks: `present`
- Binance L2/LOB: `present in runtime recording path; not separately promoted by this evidence`
- Polymarket quote ticks: `present`
- Polymarket full CLOB depth: `present`
- Official settlement: `present`
- Runtime/dry-run fills: `present`
- Data audit status: sufficient to judge current dry-run execution quality and
  runtime PnL; not sufficient to promote to live.
- Missing surfaces and impact: recorded replay parity against this dry-run
  window is still pending and blocks any live-candidate promotion.

## Accounting Semantics

- Event accounting: `one-event-one-trade`
- Entry price model: runtime BUY orders use `runtime_price_basis=full_depth_sweep`
  with `full_depth_runtime_parity=true`.
- Exit or settlement label: official settlement exits; confirmed closed rows
  are used for PnL.
- Fillability assumption: dry-run paper fills against runtime full-depth sweep
  accounting.
- Fees/slippage/latency assumption: runtime order/fill rows include fees and
  slippage; average BUY slippage was about `-0.003144`.
- Capacity or stake assumption: `15 USDT` requested stake per entry,
  `4` max positions, no daily trade cap.

## Results

- Headline metrics from the public report at
  `2026-05-27T13:39:57.928887+00:00`:
  - `total_trades=139`
  - `closed_trades=137`
  - `wins=74`, `losses=63`, `win_rate_pct=54.0`
  - `realized_pnl=-207.43`
  - `total_fees=16.43`
  - `profit_factor=0.781`
  - `sharpe=-1.0667`
  - `avg_trade=-1.5141`
  - `max_drawdown=-220.2696`
  - `open_positions=2`, `open_exposure=29.47`
- Post-reset DB window from `2026-05-27 16:08:40 +08`:
  - `closed_trades=74`
  - `wins=41`, `losses=33`, `win_rate_pct=55.41`
  - `net_pnl=-74.5452`
  - `avg_pnl=-1.0074`
  - `gross_profit=421.6964`
  - `gross_loss=496.2416`
  - `profit_factor=0.8498`
- Last two hours:
  - `closed_trades=30`
  - `wins=19`, `losses=11`, `win_rate_pct=63.33`
  - `net_pnl=-25.8234`
  - `profit_factor=0.8439`
- Symbol/side post-reset bucket behavior:
  - `BTCUSDT UP`: `22` trades, `7` wins, `15` losses,
    `net_pnl=-105.6478`, `avg_pnl=-4.8022`, `profit_factor=0.5316`
  - `ETHUSDT DOWN`: `12` trades, `9` wins, `3` losses,
    `net_pnl=-28.4443`, `avg_pnl=-2.3704`, `profit_factor=0.3699`
  - `ETHUSDT UP`: `7` trades, `5` wins, `2` losses,
    `net_pnl=11.8338`, `avg_pnl=1.6905`, `profit_factor=1.3932`
  - `BTCUSDT DOWN`: `33` trades, `20` wins, `13` losses,
    `net_pnl=47.7131`, `avg_pnl=1.4459`, `profit_factor=1.2441`
- Fill rate/capacity:
  - BUY: `139 FILLED` orders, `2085.0000` requested notional,
    `2070.5931` filled notional, `99.31%` filled notional rate.
  - SELL: `134 FILLED` orders, `1769.9054` requested and filled notional,
    `100.00%` filled notional rate.
  - Rejected BUY orders in public report: `0`.
- Runtime health:
  - `pm5d.threelayer.live`: `desired_state=paused`,
    `observed_state=paused`.
  - `pm5d.threelayer.settlement-probability-btc-eth.dryrun`:
    `desired_state=running`, `observed_state=running`.
  - `ployd.service`: `ActiveState=active`, `SubState=running`,
    `Restart=always`, `NRestarts=0`, `OOMPolicy=kill`,
    `MemoryHigh=1342177280`, `MemoryMax=1610612736`.
  - Recording file was actively growing with mtime
    `2026-05-27 21:40:57 +08`.

## Promotion Gate Check

- Hypothesis explicit: `pass`
- Data provenance recorded: `pass`
- Executable pricing conservative: `pass`
- Settlement/exit label matches lane: `pass`
- Walk-forward or leakage guard: `n/a` for this dry-run observation; prior
  research gates remain separate.
- Replay/runtime parity: `fail` for live promotion because recorded replay
  parity for this dry-run window is still pending.
- Runtime scorer/config mapping: `pass`
- Risk/stake/kill switch stated: `pass`

## Caveats

- This is a same-day dry-run window, so it is enough to reject or revise a poor
  runtime candidate but not enough to prove a replacement candidate.
- Public report aggregation can lag the latest DB settlement/order rows by a
  few minutes; DB rows were used for bucket-level diagnosis.
- Positive `BTCUSDT DOWN` and `ETHUSDT UP` buckets are not promoted by this
  note because the combined score is still negative after costs and the bucket
  split is a post-hoc diagnostic.
- This evidence does not change live deployment state; live remains paused.

## Follow-Up

- Encode this runtime feedback into a Research Manager / AutoFactor typed prior
  so the next hosted search penalizes or avoids the current losing runtime score
  shape, especially `BTCUSDT UP` and `ETHUSDT DOWN` buckets.
- Require the next candidate to pass runtime candidate replay before any dry-run
  handoff or config PR.
- Do not run recorded replay parity as a promotion step for this exact score
  unless the goal is forensic parity diagnosis; the strategy quality decision is
  already `revise`.
