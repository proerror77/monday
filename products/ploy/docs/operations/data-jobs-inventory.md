# Data Jobs Inventory

Date: 2026-05-25

This inventory classifies repo-local data scripts, jobs, and workflow surfaces so
Phase 6 cleanup can retire duplicates without guessing. It does not change live
collection behavior.

## Canonical Runtime Surfaces

| Surface | Status | Owner | Notes |
| --- | --- | --- | --- |
| `crates/ploy-market-data` | canonical | Rust data plane | Owns live feed/scanner/discovery code used by runner/runtime surfaces. |
| `/opt/ploy/bin/ploy-runner collect-quotes` / `ploy-runner-host::ops` | canonical ops surface | runner ops | Full/ops build only; lean replay/backtest binaries intentionally do not expose it. |
| `/opt/ploy/bin/ploy-runner collect-pm-trades` / `ploy-runner-host::ops` | canonical ops surface | runner ops | Polls Polymarket Data API trade prints into `clob_trade_ticks`; full/ops build only. |
| `/opt/ploy/bin/ploy-runner collect-cex-public` / `ploy-runner-host::ops` | canonical ops surface | runner ops | Collects Binance Futures public metrics/liquidations and normalized OKX, Bybit, Coinbase, and Kraken L2 rows. |
| `/opt/ploy/bin/ploy-runner collect-predict-fun` / `ploy-runner-host::ops` | canonical ops surface | runner ops | Collects Predict.fun market catalog and normalized Yes/No order-book snapshots; mainnet API key required. |
| `ploy-feed-loaders` | canonical historical DB loader | research/backtest adapters | Owns SQLx historical `MarketUpdate` loading outside strategy-bundles. |
| `scripts/export_parquet.sh` | canonical export helper | data/export host | Keep as the explicit Parquet export entrypoint until replaced by Rust datactl. |
| `.github/workflows/backtest.yml` | canonical CI backtest lane | CI/backtest host | Should remain separated from trade-host deploy assumptions. |
| `.github/workflows/optimize.yml` | canonical snapshot optimization lane | CI/backtest host | Requires a retained complete sampled research snapshot artifact. |
| `.github/workflows/deploy-trade.yml` | canonical trade-host deploy lane | ploy-trade-1 | Owns the immutable `ployd`/`ployctl`/runner bundle and stages the live deployment paused. |
| `.github/workflows/approve-live-trade.yml` | canonical live admission lane | protected human approval | The only workflow allowed to resume live after exact-SHA replay, dry-run drawdown, and strict parity evidence pass. |

## Compatibility Or Transitional Live Collection

| Surface | Status | Owner | Replacement / Direction |
| --- | --- | --- | --- |
| `scripts/binance_price_collector.py` | compatibility collector | data host | Keep until equivalent Rust collector path is proven and deployed. |
| `scripts/binance_aggtrade_collector.py` | compatibility collector | data host | Deployed by `deploy-tango-1-1.yml`; retire only after Rust feed/persistence replacement. |
| `scripts/binance_lob_collector.py` | compatibility collector | data host | Critical L2 source today; do not delete without live replacement and freshness evidence. |
| `scripts/polymarket_quote_collector.py` | compatibility collector | data host | Prefer `collect-quotes`/market-data long term. |
| `.github/workflows/deploy-tango-1-1.yml` | canonical research/data deploy | tango-1-1 | Bundles collectors, research tools, replay, and dry-run surfaces; removes live manifests, signing keys, and live gate authority. |

## One-Shot Backfill / Repair Jobs

| Surface | Status | Owner | Notes |
| --- | --- | --- | --- |
| `scripts/backfill_pm_midpoints.py` | one-shot backfill | data repair | Keep with migration/runbook context. |
| `scripts/backfill_settlements.py` | one-shot backfill | data repair | Keep until settlement repair path is Rust-owned. |
| `scripts/backfill_price_to_beat.sql` | one-shot SQL repair | DB repair | Keep as migration/reference until encoded in migration. |
| `scripts/fix_binance_lob_partitions.sql` | one-shot/operational repair | DB repair | Still deployed by `deploy-tango-1-1.yml`. |
| `scripts/fix_deribit_partitions.sql` | one-shot/operational repair | DB repair | Keep until Deribit path is retired or fully owned. |
| `scripts/verify_price_to_beat.sh` | verification helper | data repair | Keep while price-to-beat backfills are active. |
| `scripts/db_retention.sql` | maintenance SQL | DB maintenance | Keep as explicit maintenance helper. |

## Diagnostics / Reports

| Surface | Status | Owner | Notes |
| --- | --- | --- | --- |
| `scripts/check_db.sql` | diagnostic SQL | ops | Keep as SQL snippet/reference. |
| `scripts/check_polymarket_api_usage.sh` | diagnostic | ops | Keep; no Rust replacement needed. |
| `scripts/report_drawdown.py` | report helper | research | Keep until research reporting is consolidated. |
| `scripts/refresh_research_valid_windows.sh` | research maintenance | research | Keep; tied to factor research materialized view. |

## Archive Candidates

| Surface | Status | Reason |
| --- | --- | --- |
| `scripts/train_crypto_lob_tcn_onnx_from_db.py` | active research prototype | Keep while the LOB ML lane remains documented; `docs/notes/crypto_lob_ml_architecture_boundary.md` and `docs/CRYPTO_LOB_ML_DEPLOY_CHECKLIST.md` still use it as the explicit training entrypoint. |

## Platform Release / Host Install

| Surface | Status | Owner | Notes |
| --- | --- | --- | --- |
| `.github/workflows/release-platform.yml` | build-only portable bundle | CI | Produces a checksumed bootstrap artifact only. It cannot deploy; named Tango/trade workflows own host mutation. |
| `scripts/install-platform-service.sh` | portable bootstrap installer | release-platform bundle | Kept for non-host-specific bootstrap artifacts; not an Aliyun deployment authority. |

## Removed Legacy Assets

The repository no longer keeps executable legacy archives for retired
root-runtime deploy scripts, local CSV research collectors, public-profile
copy-trading prototypes, duplicate DB diagnostics, historical quote backfills,
inactive DB-backed ML trainers, or the old PM5D matrix-diagnostic workflow
family. The removed matrix family included `strategy-research-matrix.yml`,
`dryrun-correction-matrix.yml`, their `ploy-research` example binaries, and
their artifact analyzers. The old unreferenced `factor_scan` and
`collector_data_utilization` direct-DB examples were also removed. Those
diagnostics predated the current Research OS closed loop and are no longer an
active strategy-discovery path. The old live-Parquet `optimize_backtest`
example, deployed `optimize-backtest` binary path, verification gate script,
and superseded optimize verification runbook were removed after `optimize.yml`
became snapshot-only. The legacy direct-DB `factor_research` example and
deployed `factor-research` binary path were also removed; Event ML now starts
from retained event-root dataset artifacts instead of rebuilding datasets from
database access in an active workflow or deploy bundle.

Historical provenance for those retired assets is preserved in git history and
in the prior cleanup PRs referenced by the research architecture review. Do not
reintroduce them as compatibility wrappers; create a new owner, runbook, and
promotion-safe evidence path instead.

## Guardrails

- Do not delete collector scripts that are still copied or restarted by deploy workflows.
- Do not move Rust builds onto live trading hosts; deploy CI-built artifacts only.
- Prefer current Rust control-plane/runtime surfaces for new operational docs.
- Mark Python scripts as compatibility or one-shot unless they are the canonical source for a live data stream.
- Before retirement, verify runbook references with `rg` and confirm replacement freshness or parity evidence.
- Factor research no longer has a manual direct-DB break-glass runner.
  Artifact-backed research starts with `research-snapshot.yml`, routes through
  hosted factor review/walk-forward workflows, persists Research OS trace, and
  lets `research-trace-plan.yml` produce the next bounded research action.
- The old `--allow-legacy-snapshot-build` PRD-gate exception has been removed.
  Build or select retained `research-snapshot.yml` artifacts explicitly before
  dispatching promotion or handoff gates.
