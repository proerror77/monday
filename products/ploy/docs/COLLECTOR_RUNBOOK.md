# Collector And Research Data Runbook

This page routes data work through the current workspace surfaces. The old
single-binary commands such as `ploy collect`, `ploy orderbook-history`, and
`ploy strategy backfill-*` are archived compatibility references; do not use
them for new research or promotion evidence.

## Quick Routing

| Goal | Current surface | Use when | Do not use when |
| --- | --- | --- | --- |
| Deploy or restart collectors | `.github/workflows/deploy-tango-1-1.yml` | You need the production-like data plane on `tango-1-1` refreshed from CI-built artifacts | You want an ad hoc local DB command |
| Foreground collector diagnostic | `/opt/ploy/bin/ploy-runner collect-*` | You are on the data host and need to inspect one collector path with explicit DB credentials | You are producing research/promotion evidence |
| Research snapshot | `.github/workflows/research-snapshot.yml` | You need a retained sampled research artifact from remote data | You need full-depth executable replay evidence |
| Factor review / walk-forward | hosted artifact workflows | You already have a snapshot artifact and want factor attribution / walk-forward evidence | You need live trading or dry-run promotion |
| Runtime replay | `.github/workflows/runtime-candidate-replay.yml` | A runtime score needs executable `MarketUpdate` replay evidence | You only have aggregate top-bucket diagnostics |
| Data quality / gap audit | deploy postflight, `check-db`, or `scripts/audit_market_data_gaps.py` | You need freshness, coverage, or blocker evidence | You are trying to bypass a failed promotion gate |

## 1. Live Data Plane

The live data plane is owned by CI deploys and systemd units on `tango-1-1`.
Do not build Rust on the trading host and do not install one-off binaries by
hand.

The deploy workflow installs `/opt/ploy/bin/ploy-runner` plus the current
systemd units:

- `ploy-market-discovery.service`
- `ploy-quote-collector.service`
- `ploy-pm-trade-collector.service`
- `ploy-binance-price-collector.service`
- `ploy-binance-aggtrade-collector.service`
- `ploy-binance-lob-collector.service`
- `ploy-deribit-iv-collector.service`
- `ploy-deribit-greeks-collector.service`
- `ploy-cex-public-collector.service`
- `ploy-predict-fun-collector.service` (enabled only when
  `/etc/ploy/predict-fun.env` contains `PREDICT_FUN_API_KEY`)

Trigger deploys from `main`:

```bash
gh workflow run deploy-tango-1-1.yml -f git_ref=main
```

After deploy, use the workflow postflight and service logs as evidence. The
workflow checks service activity, guardrails, and collector liveness.

## 2. Foreground Collector Diagnostics

Use foreground collector commands only as operator diagnostics on the data host.
They are not the canonical research chain and should not feed promotion
artifacts directly.

Examples:

```bash
/opt/ploy/bin/ploy-runner check-db --db-url "$PLOY_DATABASE__URL"
/opt/ploy/bin/ploy-runner collect-markets --symbols BTCUSDT,ETHUSDT,SOLUSDT --db-url "$PLOY_DATABASE__URL"
/opt/ploy/bin/ploy-runner collect-quotes --symbols BTCUSDT,ETHUSDT,SOLUSDT --timeframe 5m --db-url "$PLOY_DATABASE__URL"
/opt/ploy/bin/ploy-runner collect-pm-trades --symbols BTCUSDT,ETHUSDT,SOLUSDT --db-url "$PLOY_DATABASE__URL"
/opt/ploy/bin/ploy-runner collect-binance-lob --symbols BTCUSDT,ETHUSDT,SOLUSDT --depth 20 --db-url "$PLOY_DATABASE__URL"
/opt/ploy/bin/ploy-runner collect-binance-price --symbols BTCUSDT,ETHUSDT,SOLUSDT --db-url "$PLOY_DATABASE__URL"
/opt/ploy/bin/ploy-runner collect-binance-aggtrade --symbols BTCUSDT,ETHUSDT,SOLUSDT --db-url "$PLOY_DATABASE__URL"
/opt/ploy/bin/ploy-runner collect-deribit-iv --currencies BTC,ETH,SOL --db-url "$PLOY_DATABASE__URL"
/opt/ploy/bin/ploy-runner collect-deribit-greeks --currencies BTC,ETH,SOL --db-url "$PLOY_DATABASE__URL"
/opt/ploy/bin/ploy-runner collect-cex-public --assets BTC,ETH,SOL --poll-secs 5 --sample-ms 1000 --db-url "$PLOY_DATABASE__URL"
PREDICT_FUN_API_URL=https://api-testnet.predict.fun \
  /opt/ploy/bin/ploy-runner collect-predict-fun --once --db-url "$PLOY_DATABASE__URL"
```

`collect-cex-public` writes one normalized `cex_public_market_ticks` surface:
Binance USD-M mark/index price, funding, open interest, basis and liquidation;
plus sampled L2 books from OKX `books5`, Bybit `orderbook.50`, Coinbase Advanced
Trade `level2`, and Kraken WebSocket v2 `book`. Use the `cex-extended` gap-audit
profile before consuming these rows in factor research. They are not part of
the existing PM5D promotion gate by default.

`collect-predict-fun` uses Predict.fun's official beta REST API to persist
`predict_fun_markets` and normalized `predict_fun_orderbook_ticks`. Predict's
book is Yes-based; Ploy derives No bids/asks by swapping sides and taking the
complement at the market's declared decimal precision. Mainnet rejects startup
without `PREDICT_FUN_API_KEY`; the official testnet permits keyless diagnostics.
This collector does not submit orders, hold wallet keys, or redeem positions.
Create `/etc/ploy/predict-fun.env` as a root-owned `0600` file containing only
`PREDICT_FUN_API_KEY=...`; the default 300 ms book-request spacing stays below
Predict.fun's documented 240 requests/minute key limit with catalog headroom.

If a diagnostic requires remote secrets or production data, prefer a GitHub
Actions workflow with environment-scoped secrets over a local command.

## 3. Research Snapshot Path

Research starts from retained artifacts, not ad hoc local CSV files.

```bash
gh workflow run research-snapshot.yml \
  -f git_ref=main \
  -f start_date=2026-05-16 \
  -f end_date=2026-05-18 \
  -f symbols=BTCUSDT,ETHUSDT,SOLUSDT \
  -f data_profile=pm5d-execution
```

The output is a complete sampled research snapshot artifact with manifest,
quality report, and data-gap audit. It is a factor-search input, not a claim
that every execution surface is full-fidelity.

## 4. Factor Review And Walk-Forward

Use hosted artifact workflows after a snapshot exists:

```bash
gh workflow run factor-review-v2-hosted-artifact.yml \
  -f git_ref=main \
  -f snapshot_run_id=<snapshot-run-id>

gh workflow run factor-walk-forward-v2-hosted-artifact.yml \
  -f git_ref=main \
  -f snapshot_run_id=<snapshot-run-id>
```

These requests run directly on GitHub-hosted artifact workflows. Requests
without `snapshot_run_id` fail closed instead of running direct DB research.

For AutoFactor / PM5D work, evidence stages must stay separated:

- `factor_attribution`: factor direction, stability, bucket behavior.
- `walk_forward`: train/test separation and promotion gate diagnostics.
- `runtime_parity` / `executable_replay`: runtime scorer and MarketUpdate tape
  evidence.
- `dry_run_candidate`: only after the required replay, settlement, fillability,
  risk, and trace gates pass.

## 5. Runtime Candidate Replay

Use runtime replay when a specific runtime score needs executable evidence:

```bash
gh workflow run runtime-candidate-replay.yml \
  -f deployment_id=pm5d.threelayer.settlement-probability-btc-eth.dryrun \
  -f config_path=/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml \
  -f recording_path=/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson \
  -f runtime_score=autofactor_formula:<candidate> \
  -f strategy_profile=settlement_probability \
  -f min_trade_count=50 \
  -f min_fill_rate=0.30 \
  -f min_roi=0
```

Runtime replay artifacts must report `basis=runtime_market_update_replay`.
Aggregate top-bucket candidate replay is diagnostic only and must not satisfy a
dry-run handoff.

## 6. Data Quality Checks

For quick operator checks, use:

```bash
/opt/ploy/bin/ploy-runner check-db --db-url "$PLOY_DATABASE__URL"
```

For research snapshot gaps, use the `data-gap-audit.json` and `quality.md`
uploaded by `research-snapshot.yml`. For a focused gap investigation, use
`scripts/audit_market_data_gaps.py` with an explicit remote/CI context.

Do not treat sampled snapshot rows as full-depth execution evidence. If a
promotion depends on full-depth CLOB, official settlement, or runtime replay,
missing or sampled surfaces remain blockers.

## 7. Archived Compatibility Paths

These old paths have been removed from active guidance:

- `ploy collect`
- `ploy collect --check-only`
- `ploy orderbook-history`
- `ploy deribit-iv-backfill`
- `ploy strategy backfill-*`
- `scripts/collect_data.py`
- `scripts/collect_klines.sh`
- manual deploy scripts that copy binaries onto hosts

Use archived files only for historical context when reading old evidence.
