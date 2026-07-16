# Research Backtest Rearchitecture

Date: 2026-04-28

Status: superseded

Superseded by:

- `docs/reviews/research-data-architecture-review-2026-05-25.md`
- `docs/runbooks/strategy-research-cicd.md`
- `docs/runbooks/event-ml-automl-workflow.md`

This document is preserved as the April rearchitecture proposal. It still
captures useful design intent, but it should not be used as the current
operator workflow contract. Current factor research consumes retained snapshot
artifacts through hosted workflows, persists Research OS trace, and treats
DuckDB as a Parquet query accelerator rather than durable research state.

## Problem

The current Factor V2 and walk-forward path is too close to a live production
database replay. A six-symbol, multi-day research run currently asks Tango
PostgreSQL to rebuild sampled PM books, Binance ticks, Binance LOB, Deribit IV,
Deribit Greeks, PM quotes, event metadata, and official settlement on demand.

That shape is not a reasonable high-frequency research architecture. The data
volume is not large enough to justify multi-hour runtime, but the access pattern
is wrong:

- raw collector tables are queried directly during every backtest;
- several loaders construct time buckets and do point-in-time joins against
  raw tick or option-chain tables at run time;
- JSONB order books and option-chain rows are repeatedly decoded from
  PostgreSQL instead of compiled once into a replay-ready feature tape;
- CI jobs mix data compilation, feature generation, factor review, and
  walk-forward scoring into one opaque step;
- cancellation can leave database queries or runner children alive long enough
  to continue pressuring Tango.

Recent fixes made individual queries less bad:

- PM full-depth sampling now uses token/window index lookups instead of sorting
  the full `clob_orderbook_snapshots` table.
- Binance spot and aggTrade sampling now uses bounded bucket lookups instead of
  full-range `DISTINCT ON` bucket scans.

Those fixes are still not the final architecture. The Deribit loader then
surfaced the same structural issue: the research run performs thousands of
point-in-time lookups over an option-chain source instead of consuming a compact
ATM volatility/Greeks tape.

## Design Goal

Backtests should be deterministic, point-in-time correct, executable, and fast.

For the PM5D six-symbol, seven-day research case, the target is:

- data compile job: bounded, observable, preferably under 10 minutes;
- repeated factor review or walk-forward on an existing artifact: under 2 to 5
  minutes;
- no long-running raw table scans from CI backtest jobs;
- no look-ahead labels except explicitly named future diagnostic labels;
- official Polymarket settlement is the only direction truth for resolved
  labels;
- executable PnL is computed from PM top-of-book and full-depth book snapshots,
  not only theoretical price.

## Architecture

The new design separates raw ingestion from research replay.

```text
Tango collectors / PostgreSQL raw tables
        |
        v
Research snapshot compiler
        |
        +--> canonical replay tape (Parquet/Arrow)
        +--> feature observation tape (Parquet/Arrow)
        +--> execution tape (PM quote/book sweep snapshots)
        +--> label tape (official settlement + executable PnL labels)
        +--> manifest + data-quality report
        |
        v
ploy-ci backtest/research jobs
        |
        +--> factor review
        +--> walk-forward
        +--> optimizer
        +--> report artifacts
```

Tango remains the source of truth for raw collector data. It should not be the
machine that recomputes every join for every research run. `ploy-ci-1` runs the
research binaries against immutable snapshot artifacts.

## Data Layers

### Layer 0: Raw Collector Tables

Owned by Tango collectors.

Examples:

- `binance_price_ticks`
- `binance_agg_trade_ticks`
- `binance_lob_ticks`
- `clob_quote_ticks`
- `clob_orderbook_snapshots`
- `clob_trade_ticks`
- `deribit_iv_ticks`
- `deribit_atm_greeks_ticks`
- `pm_market_metadata`
- `pm_token_settlements`

Rules:

- Raw tables are append-only source material.
- Backtest jobs may inspect freshness/counts, but must not repeatedly perform
  large joins or bucket scans over raw tables.
- Raw table indexes are still needed for snapshot compilation, collector health,
  and emergency debugging.

### Layer 1: Canonical Sampled Tapes

Compiled once per `(symbols, start, end, sample_secs, dataset_version)`.

Tapes:

- `cex_spot_30s.parquet`
- `cex_aggtrade_5s.parquet`
- `cex_lob_30s.parquet`
- `pm_quote_1s_or_30s.parquet`
- `pm_book_30s.parquet`
- `pm_trade_prints.parquet`
- `deribit_atm_iv_30s.parquet`
- `deribit_atm_greeks_30s.parquet`
- `events.parquet`
- `settlements.parquet`

These are point-in-time records. Each row must include:

- `ts`
- `symbol` or `market_slug` / `token_id`
- source timestamp and ingest timestamp when different;
- `source_lag_ms`
- nullability flags for missing data;
- `dataset_version`.

### Layer 2: Observation Tape

`FactorObservationV2` should be generated from Layer 1 tapes, not directly from
raw tables.

Required families:

- alpha and direction;
- CEX microstructure;
- PM liquidity and full-depth sweep execution;
- PM trade prints;
- Deribit IV/Greeks regime;
- continuation / candle volume regime;
- exit feasibility;
- portfolio and risk state.

The observation tape is the unit of factor review. It should be compact enough
to load fully into memory for the selected window.

### Layer 3: Label Tape

Labels are separate from features.

Required labels:

- `settlement_up`: official Polymarket settlement only;
- `entry_top_of_book_fillable_15u`;
- `entry_full_depth_fillable_15u`;
- `entry_sweep_avg_price_15u`;
- `exit_top_of_book_fillable_15u`;
- `exit_full_depth_fillable_15u`;
- `executable_pnl_top_of_book`;
- `executable_pnl_full_depth`;
- rejection reason labels.

Future-looking labels must never appear in primary factor ranking columns. They
may appear only in a diagnostic section or label tape.

### Layer 4: Research Reports

Factor review, walk-forward, and optimizer jobs consume Layer 2 and Layer 3
tapes.

They should not know how to query raw collector tables.

## Snapshot Compiler

Add a dedicated binary:

```text
research_snapshot_compile
```

Inputs:

- `--db-url`
- `--symbols`
- `--start-date`
- `--end-date`
- `--sample-secs`
- `--stake-usd`
- `--dataset-version`
- `--output-dir`
- `--require-official-settlement`

Outputs:

- Parquet/Arrow tapes;
- `manifest.json`;
- `quality.md`;
- `query_timings.json`;
- optional `EXPLAIN` samples for slow steps.

Compiler requirements:

- each source loader has a named phase and timing;
- each phase has row counts, min/max source time, null rate, and skipped reason;
- hard timeout per source phase;
- cancellation terminates the child process and database statement;
- no phase may perform unbounded `DISTINCT ON` or computed bucket scans over raw
  tables;
- Deribit must use ATM/cache tables first, not raw option-chain candidate scans.

## Backtest Runtime

Add a dedicated binary:

```text
research_walk_forward_from_snapshot
```

Inputs:

- `--snapshot-dir`
- `--train-window-days`
- `--test-window-days`
- `--step-days`
- `--top-n`
- `--top-quantile`
- optional `--factor-name-filter`

Runtime behavior:

- load observation and label tapes;
- assert manifest compatibility;
- run train/test windows in memory or streaming chunks;
- produce reports and selected factor sets;
- never connect to Tango PostgreSQL during scoring.

The runtime should remain event-driven for strategy parity where needed, but
factor review does not need to rebuild the entire raw `MarketUpdate` stream.
For Factor V2 research, observation rows plus execution/label tapes are the
canonical input.

## Deribit Redesign

Deribit is the current proof that raw replay is the wrong boundary.

Do not query `deribit_iv_ticks` raw option-chain rows per bucket during
walk-forward. Instead:

1. Prefer `deribit_atm_greeks_ticks` for ATM IV and Greeks when available.
2. Prefer `strategy_data.deribit_atm_greeks_snapshots_cache` if it is fresh and
   complete enough.
3. If raw option-chain data must be used, compile it once into an ATM tape:
   one row per `(currency, bucket_ts)`.
4. Store the resulting ATM tape as Parquet and reuse it across factor review,
   walk-forward, and optimizer jobs.

The Deribit feature contract should be:

- `currency`
- `symbol`
- `ts`
- `mark_iv`
- `bid_iv`
- `ask_iv`
- `iv_spread`
- `underlying_price`
- `delta`
- `gamma`
- `vega`
- `theta`
- `source_lag_secs`
- `source_table`
- `quality_flags`

Missing Greeks are allowed, but they must be explicit quality flags, not silent
zeroes or stale rows.

## Workflow Design

### `research-snapshot.yml`

Runs on `ploy-ci-1`.

Steps:

1. Checkout selected `git_ref`.
2. Build `research_snapshot_compile`.
3. Compile snapshot from Tango raw DB.
4. Upload snapshot artifact.
5. Publish quality report and phase timings.

Concurrency should be per `(git_ref, symbols, start, end, sample_secs)`, with
`cancel-in-progress: true` for snapshot compiles.

### Historical `factor-review-v2.yml`

This router workflow has been removed. Current factor review dispatches
`.github/workflows/factor-review-v2-hosted-artifact.yml` directly with
`snapshot_run_id`.

### Historical `factor-walk-forward-v2.yml`

This router workflow has been removed. Current factor walk-forward dispatches
`.github/workflows/factor-walk-forward-v2-hosted-artifact.yml` directly with
`snapshot_run_id`.

Missing snapshot artifacts now fail closed instead of falling back to direct DB
or compile-in-place workflow modes.

## Performance Budget

For six symbols and seven days:

- snapshot compile: target under 10 minutes;
- factor review from snapshot: target under 90 seconds;
- walk-forward from snapshot: target under 5 minutes;
- peak memory on `ploy-ci-1`: under 8 GB;
- no individual database query over 120 seconds;
- no raw table query with `BufFileRead` / temp sort in normal runs.

If a phase exceeds the budget, the job should fail with a named phase and query
fingerprint instead of continuing silently.

## Correctness Gates

Before any result is trusted:

- official settlement coverage is reported;
- unresolved events are skipped when `require_official_settlement=true`;
- observation rows have no future label columns in tradable factor rankings;
- PM executable labels report top-of-book and full-depth fill rates;
- side-aware observations cover UP-only and DOWN-only quote freshness cases;
- train/test windows are strictly time ordered;
- the snapshot manifest hash is included in every report.

## Migration Plan

### Phase 1: Stop the Bleeding

- Keep #186 PM full-depth indexed loader.
- Keep #187 Binance sampled indexed loader.
- Replace Deribit loader with ATM/cache-first loading.
- Add phase timing output to current Factor V2 examples.

### Phase 2: Build Snapshot Compiler

- Add `research_snapshot_compile`.
- Export canonical sampled tapes to Parquet.
- Add manifest and quality report.
- Add smoke compile for one day and six symbols.

### Phase 3: Move Reports to Snapshot Input

- Add `factor_review_from_snapshot`.
- Add `factor_walk_forward_from_snapshot`.
- Update GitHub Actions to use snapshot artifacts.
- Keep direct DB mode as debug-only.

### Phase 4: Optimizer

- Run Bayesian/TPE optimization only on immutable snapshot tapes.
- Store optimizer inputs, objective config, selected factors, and output params
  with the snapshot manifest hash.
- Mark all pre-snapshot optimizer results as stale.

### Phase 5: Production Parity

- Add replay parity tests between snapshot observations and live/dry-run
  execution events.
- Compare dry-run, simulated execution, and live order records on the same
  event windows.
- Only promote a strategy config when the snapshot-backed result and dry-run
  behavior agree within defined tolerances.

## Non-Goals

- Do not optimize by dropping microstructure data silently.
- Do not make Polars a substitute for point-in-time correctness.
- Do not use Binance spot-derived settlement as a label.
- Do not run full research workloads on local macOS.
- Do not build Rust on Tango trading/data hosts.

## Decision

The canonical research path should move from direct raw-DB replay to immutable
snapshot-backed replay.

Raw DB loaders remain useful for snapshot compilation, debugging, and small
smoke windows. They are no longer the normal backtest path.
