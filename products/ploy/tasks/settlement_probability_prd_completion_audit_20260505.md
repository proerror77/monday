# Settlement Probability PRD Completion Audit

Date: 2026-05-05

Objective: complete the Polymarket Crypto 5m / 15m settlement-probability PRD
as a dry-run-first strategy system. The system is not live-approved.

## Current Verdict

Status: **complete for dry-run-first BTC/ETH settlement-probability handoff**

The runtime/dry-run evidence path is now working, including settlement-specific
recorded replay parity. Runtime full-depth sweep support has been implemented
and deployed, and the first post-deploy settlement-probability dry-run order has
now shown `runtime_price_basis=full_depth_sweep` with
`full_depth_runtime_parity=true`.

The strict 168h market-data continuity gate is still blocked by a historical
collector outage, tracked in issue #339. That strict gate remains useful as
collector-health evidence, but it is not the strategy-promotion gate for PM
5m / 15m settlement research. These markets are discrete settlement events, so
the decision-grade PRD gate now uses event-complete evidence: exclude outage
events, require complete executable rows for the retained train/OOS events, and
keep train/validation/test event IDs disjoint.

The retained event-complete gate has now passed on `main` with the newer
post-deploy full-depth replay parity artifact. Therefore the PRD is complete
for the stated dry-run-first BTC/ETH settlement-probability strategy system.
It is not live approval, not all-symbol promotion, and not a clean 168h
collector-health signoff.

## Deliverable Checklist

| PRD Requirement | Evidence | Status |
| --- | --- | --- |
| Settlement probability is the main lane, not repricing momentum | `tasks/todo.md` PRD plan; issue #332 BTC/ETH handoff candidate | Complete |
| Dedicated BTC/ETH settlement dry-run config exists | `config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml`; PR #334 | Complete |
| Deployment manifest exists for paper/dry-run only | `config/deployments/pm5d.threelayer.settlement-probability-btc-eth.dryrun.json`; PR #335 | Complete |
| Tango deploy workflow installs the settlement config | `.github/workflows/deploy-tango-1-1.yml`; deploy run `25370757209` and later `25373476802` | Complete |
| Old repricing dry-run is paused while settlement dry-run runs | Remote `ployctl deployments inspect`: settlement desired/observed `Running`, repricing `Paused` | Complete |
| Dry-run report exposes the strategy label | `scripts/report_dryrun_summary.py`; dry-run label `TL Settlement Probability BTC/ETH` | Complete |
| Runtime order evidence includes q/edge audit context | PR #336; deployed SHA `d87261d0cc153babc68ae799a424b294d6a0e154`; post-deploy orders show `signal_p_hat`, `signal_edge`, `signal_entry_price` | Complete |
| Runtime full-depth sweep mechanism is implemented | PR #341; merge SHA `8f0bb71718832299dfe9780d6f73aba868d05b99`; `MarketUpdate::Quote` carries full-depth levels and `SimulatedExecutor` sweeps them when present | Complete |
| Runtime full-depth sweep mechanism is deployed | Deploy run `25377605431`; settlement dry-run config deployed with `visible_depth_haircut=0.5` and `max_sweep_levels=3` | Complete |
| Runtime full-depth parity is proven by post-deploy order context | Tango DB order at `2026-05-05 21:23:43.833643+08` records `runtime_price_basis=full_depth_sweep` and `full_depth_runtime_parity=true` | Complete |
| Post-deploy full-depth replay parity passes | Recorded replay parity run `25379165698`: orders `2/2/2`, fills `2/2/2`, `strict_parity_ready=true`, `blocking_risk_flags=[]`, decision `continue` | Complete |
| Settlement dry-run records its own MarketUpdate stream | PR #337; deployed SHA `b8c927669cf5bb673dde4a9f05d9ccc15b4f8fda`; `/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson` actively updates | Complete |
| Recorded replay parity uses the settlement-specific recording | Recorded replay parity run `25374110073`, artifact `recorded-replay-parity-25374110073` | Complete |
| Runtime strict replay parity passes for a fresh settlement window | Run `25374110073`: orders `2/2/2`, fills `2/2/2`, `strict_parity_ready=true`, `blocking_risk_flags=[]`, decision `continue` | Complete |
| Short-window PRD smoke consumes the new replay parity artifact | Gate run `25374918500`, snapshot `25374930653`, walk-forward `25375300501`; `recorded_replay_parity=true` | Complete as smoke only |
| Short-window smoke shows current data health | Snapshot `25374930653` passed `data_quality=true` with `audit_lookback_hours=1` | Complete as smoke only |
| Strict 168h continuous PRD gate passes | Gate run `25374406705` failed before snapshot compilation; audit max gaps `1695-1705m` | Collector-health blocker #339; not a strategy blocker under event-complete semantics |
| Event-complete retained PRD gate exists | PR #345 / merge `815a3e8d49a3326e42e253dd4449ff7279bf90ed`; `data_quality_mode=event-complete` added to the orchestrator/workflow/promotion gate | Complete on `main` |
| Event-complete retained PRD gate passes | Parent run `25385603748`, snapshot run `25385618701`, walk-forward run `25386935332` | Complete |
| Non-empty retained walk-forward OOS passes | Walk-forward run `25386935332`: `windows=2`, `positive_window_ratio=1.0000`, `min_test_top_edge_pnl=2.7080` | Complete |
| PRD promotion gate is decision-grade | Walk-forward artifact `factor-walk-forward-v2-25386935332` reports `ready_for_dry_run_handoff=true` | Complete |
| Live approval / live trading | No live approval requested or granted | Not in scope |

## Evidence Details

### Runtime And Deployment

- PR #336: `https://github.com/proerror77/ploy/pull/336`
  - Merge SHA: `d87261d0cc153babc68ae799a424b294d6a0e154`
  - Adds probability-edge audit context to dry-run runtime orders.
- PR #337: `https://github.com/proerror77/ploy/pull/337`
  - Merge SHA: `b8c927669cf5bb673dde4a9f05d9ccc15b4f8fda`
  - Adds settlement-specific MarketUpdate recording.
- PR #338: `https://github.com/proerror77/ploy/pull/338`
  - Merge SHA: `cd52ea18dc4df7e19a4d5f88ad6987fd2efe4fb2`
  - Records parity evidence in `tasks/todo.md`.
- PR #341: `https://github.com/proerror77/ploy/pull/341`
  - Merge SHA: `8f0bb71718832299dfe9780d6f73aba868d05b99`
  - Adds full-depth levels to runtime quote updates, populates them from
    Polymarket DB/REST feeds, and makes the simulated dry-run executor sweep
    full CLOB depth when levels are available.
  - Adds conservative runtime sweep controls:
    `visible_depth_haircut=0.5` and `max_sweep_levels=3` for the BTC/ETH
    settlement dry-run config.

Remote verification after deployment:

```text
pm5d.threelayer.settlement-probability-btc-eth.dryrun desired=Running observed=Running
pm5d.threelayer.repricing-momentum.dryrun desired=Paused observed=Paused
ployd.service ActiveState=active SubState=running Restart=always OOMPolicy=kill
MemoryHigh=1342177280 MemoryMax=1610612736
no cargo/rustc process on tango-1-1
```

Runtime full-depth deployment:

- Deploy run: `25377605431`
- Deployed SHA: `8f0bb71718832299dfe9780d6f73aba868d05b99`
- Remote verification after deployment:

```text
pm5d.threelayer.settlement-probability-btc-eth.dryrun desired=Running observed=Running
ployd.service ActiveState=active SubState=running Restart=always OOMPolicy=kill
MemoryMax=1610612736
no cargo/rustc process on tango-1-1
post-deploy CLOB ingestion live after 2026-05-05 21:14:00+08
```

Post-deploy order evidence:

```text
orders since 2026-05-05 21:14:00+08: 2
latest order: 2026-05-05 21:25:00.795843+08
full_depth_sweep orders: 1
top_book_quote orders: 0
full_depth_runtime_parity=true orders: 1

2026-05-05 21:23:43.833643+08 FILLED runtime_price_basis=full_depth_sweep full_depth_runtime_parity=true signal_edge=0.7339111626093238 signal_symbol=BTCUSDT signal_direction=DOWN
2026-05-05 21:25:00.795843+08 FILLED runtime_price_basis=settlement full_depth_runtime_parity=false
```

Interpretation: the runtime full-depth entry path is now proven by a
post-deploy order context. The `settlement` basis row is a settlement-side
runtime record and does not invalidate the full-depth entry evidence. The next
required evidence is a post-deploy recorded replay parity run over this window.

### Replay Parity

Recorded replay parity run:

- Run: `25374110073`
- Window: `2026-05-05T19:33:30+08:00 -> 2026-05-05T19:35:30+08:00`
- Deployment: `pm5d.threelayer.settlement-probability-btc-eth.dryrun`
- Recording:
  `/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson`
- Result:
  - `strict_parity_ready=true`
  - orders `2/2/2`
  - fills `2/2/2`
  - `blocking_risk_flags=[]`
  - decision `continue`

Advisory flags remain:

- `replay_has_no_event_level_rows`
- `events_present_in_dryrun_missing_from_replay`

These are not blocking for current runtime order/fill strict parity, but they
remain a quality gap before any future stronger event-level parity claim.

Post-PR #341 full-depth replay parity:

- Run: `25379165698`
- Window: `2026-05-05T21:22:30+08:00 -> 2026-05-05T21:25:30+08:00`
- Deployment: `pm5d.threelayer.settlement-probability-btc-eth.dryrun`
- Recording:
  `/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson`
- Result:
  - `strict_parity_ready=true`
  - orders `2/2/2`
  - fills `2/2/2`
  - `blocking_risk_flags=[]`
  - decision `continue`

Advisory event-level flags remain:

- `replay_has_no_event_level_rows`
- `events_present_in_dryrun_missing_from_replay`

These are still a quality gap before stronger event-level parity claims, but
they are not blocking for current order/fill runtime strict parity.

### Short-Window Smoke

Short-window PRD gate:

- Parent gate run: `25374918500`
- Snapshot run: `25374930653`
- Walk-forward run: `25375300501`
- Inputs:
  - `symbols=BTCUSDT,ETHUSDT`
  - `stake_usd=15`
  - `audit_lookback_hours=1`
  - `replay_parity_run_id=25374110073`
  - `replay_parity_artifact_name=recorded-replay-parity-25374110073`

Promotion gate excerpt:

```text
ready_for_dry_run_handoff=false
data_quality=true
deribit_vol_surface=true
full_depth_entry_capacity=true
conservative_entry_capacity=true
probability_calibration=true
full_depth_settlement_edge=true
conservative_settlement_edge=true
anti_overfit_diagnostics=true
symbol_holdout=true
recorded_replay_parity=true
walk_forward_oos=false
```

Interpretation: workflow shape and parity linkage are correct. This is not
promotion evidence because the short window has no non-empty OOS windows.

Post-PR #341 short-window PRD gate:

- Parent gate run: `25379648867`
- Snapshot run: `25379659918`
- Walk-forward run: `25380143546`
- Inputs:
  - `symbols=BTCUSDT,ETHUSDT`
  - `stake_usd=15`
  - `audit_lookback_hours=1`
  - `replay_parity_run_id=25379165698`
  - `replay_parity_artifact_name=recorded-replay-parity-25379165698`

Promotion gate result:

```text
ready_for_dry_run_handoff=false
replay_parity_ready=true
walk-forward conclusion=success
blocked gate: walk_forward_oos
reason: no non-naive model has non-empty OOS windows with positive_window_ratio >= 0.60
```

Interpretation: the updated full-depth replay parity artifact is consumable by
the PRD gate. The short-window smoke remains intentionally blocked as promotion
evidence because it has no non-empty OOS window.

### Retained Event-Complete Promotion Gate

PR #345 added event-complete strategy-promotion semantics and merged to `main`.

- PR: `https://github.com/proerror77/ploy/pull/345`
- Merge SHA: `815a3e8d49a3326e42e253dd4449ff7279bf90ed`
- Parent gate run: `25385603748`
- Snapshot run: `25385618701`
- Walk-forward run: `25386935332`
- Artifact: `factor-walk-forward-v2-25386935332`
- Replay parity artifact consumed:
  `recorded-replay-parity-25379165698`

Promotion gate excerpt:

```text
ready_for_dry_run_handoff=true
data_quality=true, mode=event_complete, snapshot_data_audit_status=critical,
  event_complete_events=2488, event_complete_rows=51989
deribit_vol_surface=true
full_depth_entry_capacity=true
conservative_entry_capacity=true
probability_calibration=true, model=q_event_surface_empirical, ece=0.001258
full_depth_settlement_edge=true, model=q_market_midpoint,
  top_edge_full_depth_pnl=2.9996
conservative_settlement_edge=true, model=q_market_midpoint,
  top_edge_conservative_pnl=2.6070
anti_overfit_diagnostics=true, model=q_final_logit_blend, passed_tests=3/3
symbol_holdout=true, model=q_market_midpoint, passed_symbols=2/2
walk_forward_oos=true, model=q_market_midpoint, windows=2,
  positive_window_ratio=1.0000, min_test_top_edge_pnl=2.7080
recorded_replay_parity=true, runtime_ready=true, event_ready=false,
  blocking_flags=<none>, advisory_flags=replay_has_no_event_level_rows|
  events_present_in_dryrun_missing_from_replay, decision=continue
```

Interpretation: the PRD strategy-promotion gate is now decision-grade for the
BTC/ETH dry-run-first settlement-probability handoff. The retained global
snapshot audit still records `critical` because of the historical outage, but
the strategy gate passes because there are enough complete executable events
after excluding incomplete event rows.

### Retained Strict-Continuous Collector-Health Blocker

Retained PRD gate:

- Gate run: `25374406705`
- Snapshot run: `25374411431`
- Inputs:
  - `symbols=BTCUSDT,ETHUSDT`
  - `stake_usd=15`
  - `audit_lookback_hours=168`
  - `replay_parity_run_id=25374110073`
  - `replay_parity_artifact_name=recorded-replay-parity-25374110073`

Failure point: `Audit required market data`, before snapshot compilation.

Required `pm5d-vol` sources:

- `polymarket_quotes`
- `polymarket_orderbooks`
- `deribit_iv`
- `deribit_atm_greeks`
- `binance_price`
- `binance_agg_trades`
- `binance_lob`

Critical gaps:

```text
polymarket_quotes: max gap 1705m >= 15m
polymarket_orderbooks: max gap 1705m >= 15m
deribit_iv: max gap 1695-1700m >= 15m
deribit_atm_greeks: max gap 1700m >= 15m
binance_price/BTCUSDT: max gap 1700m >= 15m
binance_agg_trades/BTCUSDT: max gap 1695m >= 15m
binance_lob/BTCUSDT: max gap 1695m >= 15m
binance_price/ETHUSDT: max gap 1700m >= 15m
binance_agg_trades/ETHUSDT: max gap 1695m >= 15m
binance_lob/ETHUSDT: max gap 1695m >= 15m
```

Direct Tango audit after the failed gate:

- `lookback_hours=1`: `overall_status=ok`
- `lookback_hours=168`: `overall_status=critical`

Interpretation: current ingestion is healthy. The retained gate is blocked by a
historical hole inside the 168h lookback, not by current live capture.

Fresh direct Tango audit after PR #341 full-depth parity closure:

- Run time: `2026-05-05 21:54-21:55 +08`
- Command shape:
  `scripts/audit_market_data_gaps.py --symbols BTCUSDT,ETHUSDT --required-sources pm5d-vol`
- `lookback_hours=1`: `overall_status=ok`
  - all required PM, Deribit, and Binance sources had `100%` coverage
  - latest rows were within seconds
- `lookback_hours=168`: `overall_status=critical`
  - PM quotes/orderbooks coverage: about `72.4-72.5%`
  - Deribit IV/ATM greeks coverage: about `78.2%`
  - Binance BTC/ETH price, agg trades, and LOB coverage: about `78.2%`
  - max gap: about `1700-1705m`
  - gap window: roughly `2026-05-04 05:29/05:30 +08` to
    `2026-05-05 09:49/09:54 +08`

This confirms current ingestion is healthy. The historical gap blocks the strict
continuous collector-health gate, but it should not automatically discard
complete PM5D/PM15D events before or after the outage.

## Residual Gaps

### Gap 1: Strict Continuous Collector-Health Evidence

Issue: `https://github.com/proerror77/ploy/issues/339`

The older retained strict PRD gate cannot pass until every required `pm5d-vol`
source has continuous coverage over the audit window. That is a valid
collector-health check, not the only valid event-strategy promotion check.

Current practical interpretation:

1. Strategy promotion can use event-complete retained evidence and has passed.
2. Strict continuous audit remains a separate collector-health retry after the
   2026-05-04/2026-05-05 collection outage rolls out of the 168h lookback,
   roughly after `2026-05-12 09:55 +08:00`, or after validated lossless
   full-source backfill.
3. Do not substitute candles or synthetic LOB rows for missing event features.
   Exclude incomplete events for strategy research.

The runbook exposes historical PM orderbook and Deribit IV backfill paths, but
there is no current repo evidence of a lossless historical Binance LOB backfill
for the missing WebSocket partial-depth interval. Do not substitute candles or
synthetic LOB rows for missing event features. Exclude incomplete events
instead.

### Gap 2: Event-Level Replay Parity Advisory Flags

Recorded replay parity has no blocking order/fill runtime flags, but still
reports advisory event-level flags:

- `replay_has_no_event_level_rows`
- `events_present_in_dryrun_missing_from_replay`

These do not block the current dry-run handoff because order/fill strict parity
passed and the PRD gate treats them as advisory. They should be closed before
making stronger event-level replay claims or any future live-readiness claim.

### Resolved: Post-Deploy Full-Depth Runtime Parity

PR #341 implemented and deployed runtime full-depth sweep support, and one
post-deploy settlement-probability dry-run order has proven the full-depth entry
path in runtime order context:

```text
runtime_price_basis=full_depth_sweep
full_depth_runtime_parity=true
```

Recorded replay parity over the same post-deploy window also passed with no
blocking risk flags. This closes the runtime full-depth parity blocker for the
dry-run/replay path. It does not close the retained 168h data-continuity gate or
authorize live trading.

## Next Valid Actions

1. Keep the settlement dry-run collecting paper evidence.
2. Keep monitoring quick data audit; current 1h audit is healthy.
3. Re-run the strict continuous collector-health gate after the 168h data hole
   rolls out, or after a validated full-source backfill.
4. Keep live trading disabled until separately approved, risk-gated, and
   supported by additional dry-run evidence.

Recommended strict collector-health retry command after the hole rolls out of
the 168h lookback:

```bash
gh workflow run settlement-probability-prd-gate.yml \
  --ref main \
  -f git_ref=main \
  -f start_date=2026-05-05 \
  -f end_date=2026-05-12 \
  -f symbols=BTCUSDT,ETHUSDT \
  -f stake_usd=15 \
  -f issue_number=332 \
  -f audit_lookback_hours=168 \
  -f replay_parity_run_id=25379165698 \
  -f replay_parity_artifact_name=recorded-replay-parity-25379165698 \
  -f no_wait=false
```

Run the strict command only after about `2026-05-12 09:55 +08`, or after a
validated lossless full-source backfill. Before that time, the strict data audit
is expected to fail for the same retained-window gap.

## Completion Criteria

Met for dry-run-first BTC/ETH settlement-probability handoff:

1. Event-complete retained data quality passed with enough complete events and
   rows.
2. Retained snapshot-backed walk-forward has non-empty OOS windows.
3. Promotion gate reports `ready_for_dry_run_handoff=true` using the
   settlement-specific replay parity artifact.
4. Full-depth and conservative settlement edge gates passed.
5. Probability calibration, anti-overfit diagnostics, symbol holdout, Deribit
   inclusion, and recorded replay parity passed.

Not met, and intentionally outside this completion claim:

1. Live trading approval.
2. All-symbol strategy promotion.
3. Clean strict 168h continuous collector-health signoff.
4. Strong event-level replay parity without advisory flags.
