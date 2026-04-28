# Binance Market Data Refactor Execution Plan

Date: 2026-04-28
Status: execution plan

This plan narrows the current project from a broad "HFT platform" refactor into
a concrete Binance low-latency market-data and signal-engine lane.

The target for the next phase is:

```text
Binance raw depth frame
  -> parser / normalizer
  -> snapshot + diff sequence bridge
  -> local correctness book
  -> TopN feature view
  -> microstructure features
  -> expiring signal
  -> replay / paper-trading evaluation
```

Live execution, cross-exchange arbitrage, generic OMS cleanup, and SBE/custom
parser work are intentionally deferred until this lane is correct, replayable,
and measured.

## Current Repo Facts

The repo already contains the right starting point:

- `market-core/engine/src/binance_md/` is the canonical fast-lane module.
- `parser.rs` uses typed serde structs and fixed-point decimal parsing instead
  of `serde_json::Value` or `f64`.
- `book_sync.rs` models Binance snapshot/diff bridge states and gap detection.
- `book.rs` separates a full correctness book from `TopBook<N>`.
- `features.rs` already computes OBI, microprice, spread, microgap, and
  staleness.
- `signal.rs` emits expiring rule-based signals.
- `replay.rs` defines replay records, but not a durable replay writer/reader.
- CI has a focused `hft-engine` Binance MD lane:
  `cargo check -p hft-engine --locked` and
  `cargo test -p hft-engine --locked binance_md`.

The main gap is not architecture imagination. The gap is turning this fast lane
into a tested, executable, replayable boundary and preventing the generic
platform from leaking back into the hot path.

## Non-Goals For This Refactor

- Do not optimize generic multi-venue runtime first.
- Do not wire strategy directly to live orders.
- Do not make `risk-control` or `execution-gateway` part of the P0 hot path.
- Do not introduce new dependencies unless benchmark evidence requires them.
- Do not replace typed JSON with SIMD/custom/SBE parsing before baseline p99 is
  measured.
- Do not claim 40ns for WebSocket, JSON parse, or raw frame to signal.

## Target Module Boundary

Keep the fast lane under `hft-engine` first:

```text
market-core/engine/src/binance_md/
  raw.rs          # receive timestamp + borrowed/fixed-capacity frame surfaces
  parser.rs       # typed JSON baseline + fixed-point normalization
  book_sync.rs    # Binance snapshot/diff sequence state machine
  book.rs         # correctness book + TopBook<N>
  features.rs     # OBI/microprice/spread/staleness
  signal.rs       # rule-based expiring signal only
  latency.rs      # per-stage trace + histogram input
  replay.rs       # replay record schema + deterministic replay driver
  pipeline.rs     # raw/update -> book -> feature -> signal orchestration
```

Add executable boundaries only after the library lane is locked:

```text
apps/binance-md/
  src/main.rs     # WebSocket receiver, snapshot fetch, stats, replay log

apps/replay/
  src/binance_md.rs or subcommand
                 # deterministic Binance MD replay

apps/paper/
  src/binance_md.rs or subcommand
                 # signal evaluation without live orders
```

## Cleanup Plan

Behavior must be locked before cleanup edits.

1. Behavior lock
   - Keep existing unit tests for parser, book sync, book, features, and signal.
   - Add end-to-end lane tests before structural cleanup:
     raw depth frames + snapshot bridge -> final book -> feature -> signal.
   - Add replay parity tests before changing replay or persistence code.

2. Remove wrong boundaries
   - Keep Binance MD signal generation independent from generic
     `Strategy -> Risk -> Execution`.
   - Keep `ports::MarketEvent`, Decimal-heavy surfaces, and string symbols out
     of the local hot path.
   - Keep Tokio and REST calls outside `MarketDataLane`.

3. Reduce duplicate planning/docs
   - Treat `docs/architecture/BINANCE_LOW_LATENCY_MARKET_DATA_PLAN.md` as the
     high-level blueprint.
   - Treat this file as the execution checklist.
   - Archive or de-prioritize broad platform refactor docs when they conflict
     with this lane.

4. Optimize only after measurement
   - First measure typed JSON + `CorrectnessBook<N>`.
   - Then remove measured allocations and clone points.
   - Only then consider fixed-capacity depth arrays, SIMD JSON, custom parsing,
     SPSC ring buffers, SBE, or CPU pinning.

## Phase 0: Baseline And Guardrails

Goal: make the existing fast lane trustworthy before adding live I/O.

Tasks:

- [x] Add a lane-level test that applies a snapshot and buffered diffs, then checks
  `best_bid`, `best_ask`, `last_update_id`, feature fields, and signal expiry.
- [x] Add a gap test that proves a sequence break returns `RebuildRequired` and
  does not emit feature/signal output.
- [x] Add a replay-shape test proving raw/depth/feature/signal records carry the
  expected update ids and sync state.
- [x] Decide the canonical symbol ids for BTCUSDT and ETHUSDT in test fixtures.
- [x] Keep `cargo test -p hft-engine --locked binance_md` as the first gate.

2026-04-28 progress:

- `pipeline.rs` now has a deterministic BTCUSDT fixture id and lane-level
  assertions for snapshot bridge, feature fields, signal expiry, sequence gap
  behavior, and replay payload shape.
- `MarketDataLane` now exposes `process_raw_depth_frame_with_clock` and
  `process_depth_update_with_clock` so the future receiver can stamp
  parse/book/feature/signal stages without moving Tokio or logging into the hot
  path.

Acceptance:

- The library lane can prove correct snapshot bridging without network.
- Invalid ranges and gaps never update the book.
- Signals are always timestamped and expiring.

## Phase 1: Dedicated Binance Receiver App

Goal: receive live Binance depth data without contaminating the hot path.

Tasks:

- [x] Add `apps/binance-md` as a small binary.
- [x] Tokio owns WebSocket and REST snapshot fetch only.
- [x] The engine lane receives raw bytes through a bounded queue or direct single
  owner loop.
- [ ] Implement full Binance connection lifecycle rules in the cold/control path:
  24h reconnect, ping/pong handling, reconnect backoff, and snapshot rebuild.
- [x] Use `timeUnit=MICROSECOND` in stream URLs where supported.
- [x] Add a bookTicker sanity checker outside the hot path.

2026-04-28 progress:

- `apps/binance-md` provides `live`, `replay`, and `paper` subcommands.
- Live smoke connected to `wss://stream.binance.com:9443/stream`, buffered the
  first diff, fetched REST snapshot from `/api/v3/depth`, bridged to live state,
  processed one depth update, checked bookTicker mismatch count, wrote replay
  NDJSON, and emitted latency histograms.
- Full reconnect/backoff/24h lifecycle remains a follow-up; the current binary
  handles close/end by exiting and gap by snapshot reload.

Acceptance:

- BTCUSDT/ETHUSDT depth frames are timestamped at receive.
- Snapshot bridge reaches `Live`.
- Gap/reconnect triggers rebuild.
- Local best bid/ask sanity-checks against bookTicker.

## Phase 2: Latency Trace And Histogram

Goal: make p99/p999/max visible for the local raw-frame-to-signal path.

Tasks:

- [x] Stamp `parse_done_ns`, `book_done_ns`, `feature_done_ns`, and
  `signal_done_ns` inside the pipeline boundary or immediately around it.
- [x] Feed stage durations into `hdrhistogram`.
- [x] Report p50, p95, p99, p999, and max for parse/book/feature/signal/total.
- [ ] Include commit SHA, host, CPU model, build profile, and feature flags in
  benchmark output.
- [x] Keep stdout/logging off the hot path; aggregate and print on intervals.

Acceptance:

- Phase 1 target: local raw frame -> signal p99 < 1ms in release mode.
- Every latency claim includes host/config/build evidence.
- Debug-build latency is never used as evidence.

## Phase 3: Replay As The Contract

Goal: same input produces same book/features/signals.

Tasks:

- [x] Add durable replay writer for raw frame, snapshot, bridge, depth, feature,
  and signal records.
- [x] Add replay reader that reconstructs the lane from recorded events.
- [x] Add deterministic replay parity tests:
  final book, feature sequence, signal sequence, and gap/rebuild decisions.
- [ ] Version replay records so later schema changes are explicit.

2026-04-28 progress:

- `ReplayRecord` and dependent sync/feature/signal/latency types are serde
  serializable.
- `write_replay_batch` writes NDJSON and `read_replay_records` reads it back.
- `hft-binance-md replay --replay-in ...` reconstructs snapshot/depth records
  through the same `MarketDataLane`.
- Bridge records now persist the buffered diff payloads that were applied while
  connecting the REST snapshot to the live stream, so initial bridge replay can
  reproduce the advanced book state instead of only replaying bridge metadata.
- Replay reports parity mismatches when recomputed feature/signal output differs
  from the persisted feature/signal records.

Acceptance:

- Live capture can be replayed offline.
- Replay reproduces feature/signal outputs for the same input.
- Paper trading consumes replay, not only live stream state.

## Phase 4: Paper Trading, Still No Live Orders

Goal: evaluate whether the signal has edge before any live execution.

Tasks:

- [x] Convert `Signal` to paper-only evaluation in the Binance MD app.
- [x] Simulate one-signal-to-next-signal paper fills with fees and slippage.
- [ ] Simulate market fills, limit fills, partial fills, queue
  position, order latency, and cancel delay.
- [ ] Add kill-switch simulation for stale data, sequence gap, latency spike,
  max loss, max position, max order rate, and rejects.
- [ ] Report hit rate, realized spread, turnover, and
  max adverse excursion.
- [x] Report signals, long/short count, realized trades, gross PnL, fees,
  slippage, net PnL, and max drawdown.

Acceptance:

- Strategy quality is evaluated from replay/paper evidence.
- No live order path is enabled by this refactor.

## Phase 5: Optimization Ladder

Only start after Phases 0-3 have evidence.

Order:

1. Remove measured hot-path clones in replay capture.
2. Replace `Vec<(i64, i64)>` depth updates with reusable buffers if allocation
   shows up in p99/p999.
3. Avoid full `BTreeMap` top refresh per diff if book update time dominates.
4. Introduce pinned engine thread after baseline.
5. Replace queue only if bounded channel cost is proven significant.
6. Try `simd-json` or `sonic-rs` only if parser dominates.
7. Custom parser only after typed/SIMD parser evidence.
8. SBE only after JSON lane is correct and replayable.

Targets:

- JSON baseline: p99 < 1ms.
- Optimized JSON: p99 < 300us.
- SBE/custom parser path: p99 < 100us.
- OBI/threshold functions may target 40ns, but complete raw-frame-to-signal
  should not use 40ns as its target.

## Verification Commands

Standard local gate:

```bash
cd rust_hft
cargo fmt --check --package hft-binance-md --package hft-engine --package hft-data-adapter-binance
cargo check -p hft-engine --locked
cargo test -p hft-engine --locked binance_md
cargo check -p hft-binance-md --locked
cargo test -p hft-binance-md --locked
cargo check -p hft-data-adapter-binance --locked
cargo test -p hft-data-adapter-binance --locked
git diff --check
```

Release benchmark gate:

```bash
cd rust_hft
RUSTFLAGS="-C target-cpu=native" cargo bench -p hft-engine --bench engine_hotpath
```

Remote or CI benchmark output must include:

- commit SHA
- host/provider/region
- CPU model
- kernel/OS
- Rust version
- build profile
- feature flags
- p50/p95/p99/p999/max

## First PR Slice

The first implementation slice should be deliberately small:

1. [x] Add lane-level integration tests for snapshot bridge -> book -> feature ->
   signal.
2. [x] Add gap/no-signal tests.
3. [x] Add replay parity test scaffolding.
4. [x] Add stage timestamp stamping or a clear wrapper that owns it.
5. [x] Run the focused `hft-engine` Binance MD gate.

Do not add the live receiver app in the same PR unless the test gate is already
stable. The receiver depends on correctness; correctness should not depend on
the receiver.

## 2026-04-28 Validation Snapshot

Live smoke:

```bash
cd rust_hft
rm -f /tmp/binance-md-smoke.ndjson
perl -e 'alarm shift; exec @ARGV' 45 \
  cargo run -p hft-binance-md --locked -- live \
  --symbol BTCUSDT \
  --max-messages 1 \
  --max-runtime-secs 20 \
  --replay-out /tmp/binance-md-smoke.ndjson
cargo run -p hft-binance-md --locked -- replay \
  --replay-in /tmp/binance-md-smoke.ndjson
cargo run -p hft-binance-md --locked -- paper \
  --replay-in /tmp/binance-md-smoke.ndjson
```

Observed smoke result:

- WebSocket connected with `timeUnit=MICROSECOND`.
- REST depth snapshot returned HTTP 200 with 50 bid/ask levels.
- Snapshot bridge reached `Live`.
- `bookTicker_mismatch=0`.
- Replay file contained `Snapshot`, `Bridge`, `RawFrame`, `DepthUpdate`,
  `Feature`, and `Signal` records.
- Replay summary reported `parity_mismatches: 0`.
- Paper summary loaded the same replay file and produced signal/PnL counters.

Known remaining follow-ups:

- Full reconnect/backoff/24h lifecycle is not complete.
- Paper trading is a simple signal-to-next-signal evaluator, not a market/limit,
  partial-fill, queue-position, latency, cancel-delay simulator.
- Replay schema versioning still needs an explicit version field before wider
  persisted-data compatibility is promised.
