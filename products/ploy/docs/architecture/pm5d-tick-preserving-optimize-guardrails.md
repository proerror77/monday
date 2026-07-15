# PM5D Tick-Preserving Optimize Guardrails

Date: 2026-04-23

Status: superseded by snapshot-only optimization in
`docs/runbooks/strategy-research-cicd.md`

This document is retained as historical context for the removed
`optimize_backtest` live-Parquet path. The former nested optimization workflow was
also removed; current PLOY validation is owned by the Monday repository-root
`.github/workflows/ploy-ci.yml` and does not restore that optimizer lane.

## Context

The PM5D optimize workflow made `ploy-ci-1` unreachable during the Apr 15-22
multi-symbol Parquet run. The failure mode was resource exhaustion around a
large replay, not a strategy decision failure. The recovery work must keep the
market-data semantics intact: Binance spot ticks, `aggTrade`, LOB depth,
Polymarket quotes, and event lifecycle updates remain full-cadence replay
inputs.

The user explicitly rejected downsampling LOB or `aggTrade` as the fix. Any
optimizer guardrail must fail before an unsafe replay starts, or stream the same
ticks with bounded resource use. It must not silently sample, aggregate, or omit
microstructure events.

## Decision

Use resource guardrails and faithful streaming before adding faster
approximation layers:

- Add a cheap preflight/manifest phase before heavy replay.
- Bound DuckDB/process behavior with explicit memory, temp-directory, timeout,
  and host-health reporting controls.
- Remove Parquet full-window train/validation `Vec<MarketUpdate>` /
  `Arc<[MarketUpdate]>` materialization from the optimizer path.
- Keep DB eager replay scoped and labeled separately from Parquet streaming.
- Surface background Parquet/DuckDB feed failures as optimizer failures.
- Add parity tests before replacing the current timestamp ordering or adding a
  chunked k-way merge feed.
- Treat feature-tape acceleration as a later ADR after faithful raw-tick replay
  is safe and measured.

## Non-Negotiable Replay Contract

The canonical PM5D optimizer replay must preserve:

- one emitted update per source row for spot and `aggTrade`;
- LOB source rows without time downsampling;
- dual LOB emission order when one row produces both `L2` and `L2Depth`;
- deterministic same-timestamp ordering across spot, `aggTrade`, LOB, PM quote,
  and event lifecycle updates;
- PM quote filtering/trusted-source behavior from the historical feed path;
- settlement lifecycle behavior, including unresolved-event handling.

Any `--max-updates` or smoke limit is non-canonical validation only. It may prove
the binary and workflow wiring, but it cannot be reported as a production-scale
optimization result.

## Runbook

Before starting a full optimize run:

1. Run the preflight/manifest step for the exact requested symbols and date
   windows.
2. Confirm the manifest prints source-level counts for spot, `aggTrade`, LOB,
   PM quotes, events, estimated bytes, and split coverage.
3. Confirm the run is below configured row/byte/symbol/day thresholds, or that
   `--allow-large-window` is explicit and justified in the job summary.
4. Confirm DuckDB temp/spill output is isolated to a run-specific directory that
   can be cleaned at job end.
5. Confirm process timeout and post-run host-health steps are active.

Safe validation sequence:

1. Preflight-only on the Apr 15-22 six-symbol window.
2. Bounded optimize smoke on two symbols, one-day train/validation, five trials.
3. Narrow six-symbol smoke on a short timestamp window, one to three trials.
4. Only scale toward the original Apr 15-22 six-symbol request after the runner
   remains online and idle after each bounded run.

Post-run host-health evidence should record:

- GitHub runner online/busy state;
- no lingering `Runner.Worker`, `optimize_backtest`, `duckdb`, `cargo`, or
  `rustc` process from the run;
- memory/swap/disk state;
- DuckDB temp/spill directory cleanup result;
- manifest update counts and trial throughput.

## Job Summary Text

Each optimize workflow run should append a compact summary with this shape:

```markdown
## PM5D optimize guardrails

- Replay mode: faithful raw-tick Parquet streaming / DB eager / smoke-limited
- Canonical result: yes/no
- Symbols: ...
- Train: ... to ...
- Validation: ... to ...
- Source counts: spot=..., aggTrade=..., LOB=..., quotes=..., events=...
- Estimated rows/bytes: ...
- Guardrail decision: pass/fail/override
- DuckDB temp dir: ...
- Timeout: ...
- Trials completed: ...
- Updates processed: ...
- Runner health after run: online=..., busy=..., lingering_processes=...
- Notes: LOB and aggTrade were not downsampled.
```

If a guardrail blocks the run, the summary should still include the manifest and
the exact threshold that failed. A blocked preflight is a successful safety
outcome when it prevents runner loss.

## Integration Checklist

For the team lead merge/cherry-pick:

- Confirm workflow and optimizer CLI flags have matching names.
- Confirm preflight can run without starting the optimizer replay loop.
- Confirm Parquet optimizer mode no longer builds full-window train/validation
  `Vec<MarketUpdate>` or `Arc<[MarketUpdate]>`.
- Confirm DB eager behavior is unchanged or explicitly tested if changed.
- Confirm `StreamingParquetFeed` background errors fail the optimizer.
- Confirm same-timestamp and LOB dual-emission parity tests exist before any
  chunked merge implementation becomes canonical.
- Confirm `tasks/todo.md` includes the final bounded-run evidence before
  attempting the original large window again.
- Confirm docs and workflow summary text never describe smoke-limited or
  `--max-updates` runs as canonical optimization results.

## Rejected Alternatives

- Downsample LOB or `aggTrade`: rejected because it changes the signal the
  strategy is intended to evaluate.
- Add swap and keep eager full-window replay: rejected because the previous host
  failure already showed the runner can become unreachable under this shape.
- Jump directly to feature tapes: rejected until faithful raw-tick replay has
  parity tests and bounded-resource evidence.

## Consequences

Initial guarded runs may be slower than the old eager path. That is acceptable
because the first recovery requirement is preserving replay correctness while
keeping `ploy-ci-1` reachable. If faithful streaming remains too slow or DuckDB
global sorting still spills unsafely, the next architecture slice is a measured
chunked k-way Parquet merge feed with parity tests as the gate.
