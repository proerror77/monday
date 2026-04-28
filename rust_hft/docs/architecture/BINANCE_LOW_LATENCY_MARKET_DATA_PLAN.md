# Binance Low-Latency Market Data + Signal Engine Plan

Date: 2026-04-28
Status: Draft, supersedes the generic "HFT system first" plan for the next execution phase.

## Executive Correction

The current architecture is too broad for the next low-latency milestone. It is organized as a general multi-venue trading system with adapters, engine, strategies, risk, execution, runtime, apps, metrics, and deployment all evolving together. That is useful for a full trading platform, but it is not the right first target for a Binance low-latency path.

The next milestone should not be framed as "build an HFT system". It should be framed as:

```text
Binance Low-Latency Market Data + Signal Engine in Rust
```

Scope for the first lane:

```text
Binance raw market data
  -> local order book correctness
  -> microstructure features
  -> signal generation
  -> latency trace
  -> replay
  -> paper trading
```

Execution, cross-venue arbitrage, full risk routing, live order management, and generic adapter abstractions are explicitly downstream. They must not drive the P0/P1 design.

## Why The Current Plan Does Not Fit

The existing plan and codebase already contain valuable components, but several decisions are mismatched with the Binance low-latency objective:

1. The current repo is multi-venue first. The fast path should be Binance-only until book correctness, replay, and p99 latency are proven.
2. Existing `MarketEvent` and strategy interfaces are platform-oriented. They carry flexible symbols, vectors, snapshots, and trait boundaries that are convenient but not ideal for the local hot path.
3. Existing latency monitoring is shared-service oriented. The strict hot path should not record each stage through shared locks or heavyweight maps.
4. The old `002-orderbook-rework` plan targets a generic BTreeMap/Decimal order book. For Binance low-latency processing, use a correctness book plus a fixed-size top-N view; feature computation should use fixed-point integers.
5. The engine currently routes toward Strategy -> Risk -> Execution too early. For this lane, strategy output should be `Signal` first, not live `OrderIntent`.
6. Replay is currently not the primary system contract. For market microstructure work, replay must be a first-class acceptance gate.

## Non-Negotiable Principles

1. Correctness before speed: local order book sequence handling must be exact before optimizing parser or queues.
2. Local latency targets must be scoped from "raw frame received by our process" to "signal emitted"; do not include exchange push interval or public network RTT in the 100us target.
3. Hot path must avoid `await`, REST, DB writes, `println!`, `format!`, `to_string`, `serde_json::Value`, unbounded channels, per-message heap allocation, and full snapshot cloning.
4. Strategy must not place orders. It emits `Signal` first. Later phases may convert signal to `OrderIntent`, then pass through risk.
5. Every market-data decision must be replayable from recorded input plus snapshot-sync context.
6. Benchmark and soak tests belong on CI or a remote Linux host, not on a laptop by default.

## Corrected Target Architecture

```text
Binance WebSocket JSON/SBE
        |
        v
Raw Frame Receiver
        |
        v
Binance Parser / Normalizer
        |
        v
Book Sync State Machine
  - buffer diff events
  - REST snapshot
  - bridge first usable update
  - detect gaps
  - trigger rebuild
        |
        v
Local Order Book
  - correctness book
  - TopN fast view
        |
        v
Feature Engine
  - OBI1 / OBI3
  - microprice / microgap
  - spread
  - trade flow
  - staleness
        |
        v
Signal Engine
  - side
  - confidence
  - edge
  - expiry
        |
        v
Replay / Paper Trading
```

Execution is not in the first hot path. It is a later consumer of signal output.

## Repo Mapping

Use the existing workspace, but add a narrow fast lane instead of rewriting the whole platform.

Recommended new modules/crates:

```text
rust_hft/
  market-core/
    core/
      src/
        fixed_point.rs              # keep and harden; use integer fixed-point in hot path
        md_event.rs                 # new compact event structs if needed
    engine/
      src/
        binance_md/                 # new narrow lane inside engine first
          raw.rs                    # RawFrame / RawFrameRef
          parser.rs                 # typed JSON parser first
          book_sync.rs              # Binance snapshot + diff bridge state machine
          book.rs                   # correctness book + TopN view
          features.rs               # OBI/microprice/microgap/spread
          signal.rs                 # signal rules and expiry
          latency.rs                # per-message trace, low overhead
          replay.rs                 # deterministic replay surface
  apps/
    binance-md/                     # later: dedicated binary for this lane
```

Keep existing generic modules:

```text
data-pipelines/adapters/adapter-binance
market-core/engine
strategy-framework/*
risk-control/*
execution-gateway/*
apps/live
apps/paper
apps/replay
```

But do not optimize the generic runtime first. It is downstream of the Binance market-data fast lane.

## Data Model Correction

Hot-path structs should be compact and ID-based.

```rust
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct MdEvent {
    pub symbol_id: u32,
    pub exchange_id: u16,
    pub event_type: u16,
    pub side: u8,
    pub price: i64,
    pub qty: i64,
    pub first_update_id: u64,
    pub final_update_id: u64,
    pub exchange_ts_ns: i64,
    pub receive_ts_ns: i64,
}
```

Feature output should stay copyable:

```rust
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct FeatureSnapshot {
    pub symbol_id: u32,
    pub mid: i64,
    pub spread: i64,
    pub obi1_fp: i64,
    pub obi3_fp: i64,
    pub microgap: i64,
    pub flow_1s: i64,
    pub ts_ns: i64,
}
```

Signals must expire:

```rust
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Signal {
    pub symbol_id: u32,
    pub side: u8,
    pub confidence_fp: i64,
    pub edge_bps: i32,
    pub expire_ts_ns: i64,
}
```

## Order Book Correction

Do not choose one structure for every purpose.

Use two surfaces:

1. Correctness book
   - can represent the Binance snapshot and diff update semantics.
   - owns `last_update_id`.
   - detects gaps and stale events.
   - is allowed to be less "pretty" if it is exact.

2. TopN fast view
   - fixed-size `OrderBook<50>` or `TopBook<50>`.
   - feeds OBI, microprice, spread, staleness, and signal rules.
   - uses fixed-point integer price/quantity.

The old "BTreeMap + Decimal everywhere" plan is not the low-latency target. It may be useful as a correctness fallback or cold-path representation, but feature computation should not depend on Decimal or dynamic maps.

## Latency Budget

Budgets must be explicit:

```text
Local raw frame -> signal:
  Phase 1: p99 < 1ms
  Phase 2: p99 < 300us
  Phase 3: p99 < 100us

Function-level budget:
  OBI / threshold / simple risk compare: can target ~40ns

Not 40ns targets:
  WebSocket receive
  JSON parse
  SBE frame decode
  public network RTT
  exchange order ACK
```

Track at least:

```text
recv_ns
parse_done_ns
book_done_ns
feature_done_ns
signal_done_ns
```

Report:

```text
p50, p95, p99, p999, max
```

The max and p999 matter more than mean latency.

## Corrected 30-Day Plan

### Week 1: Binance Market Data + Book Correctness

Goal: local order book is correct and recoverable.

Tasks:

- [ ] Build Binance raw WebSocket receiver for BTCUSDT/ETHUSDT depth.
- [ ] Record receive timestamp for every raw frame.
- [ ] Parse typed Binance depth JSON without `serde_json::Value`.
- [ ] Implement Binance book sync state machine:
  - [ ] open stream and buffer diff events.
  - [ ] fetch REST snapshot.
  - [ ] discard old buffered events.
  - [ ] find first event whose range bridges snapshot `lastUpdateId`.
  - [ ] apply continuous diffs.
  - [ ] rebuild on update gap.
- [ ] Maintain correctness book and top-N view.
- [ ] Compare local best bid/ask against Binance bookTicker for sanity.
- [ ] Record replay log with raw frame plus sync context.

Acceptance:

- [ ] best bid/ask stays sane against bookTicker.
- [ ] update id gaps are detected.
- [ ] reconnect triggers rebuild.
- [ ] replay can reconstruct the same book state.

### Week 2: Feature + Signal

Goal: every valid book update can produce deterministic features and optional signal.

Tasks:

- [ ] Implement OBI1 and OBI3 with fixed-point integer math.
- [ ] Implement microprice and microgap.
- [ ] Implement spread and book staleness.
- [ ] Implement 1s/5s/10s trade flow if trade stream is enabled.
- [ ] Emit `FeatureSnapshot`.
- [ ] Implement simple rule-based `Signal`.
- [ ] Add signal expiry.
- [ ] Record feature and signal replay output.

Acceptance:

- [ ] no strategy path places orders.
- [ ] signal always has timestamp and expiry.
- [ ] live and replay produce the same feature/signal sequence for the same input.

### Week 3: Measurement + Optimization

Goal: identify real bottlenecks before adding complexity.

Tasks:

- [ ] Add per-stage latency trace.
- [ ] Add histogram reporting for p50/p95/p99/p999/max.
- [ ] Add remote benchmark workflow or remote Linux runbook.
- [ ] Remove hot-path allocations found by benchmark.
- [ ] Replace typed JSON parser only if parser is proven bottleneck.
- [ ] Introduce pinned engine thread after baseline measurement.
- [ ] Keep Tokio at network boundary only.

Acceptance:

- [ ] Phase 1 local raw frame -> signal p99 < 1ms.
- [ ] all benchmark results include commit SHA, host, CPU governor, target, and config.
- [ ] no local laptop build/bench is required for standard review.

### Week 4: Replay + Paper Trading

Goal: evaluate signals without live execution.

Tasks:

- [ ] Implement deterministic replay engine.
- [ ] Compare live vs replay book/features/signals.
- [ ] Implement paper order model:
  - [ ] market fill.
  - [ ] limit fill.
  - [ ] partial fill.
  - [ ] fees.
  - [ ] slippage.
  - [ ] latency/cancel delay.
- [ ] Add kill-switch simulation:
  - [ ] stale data.
  - [ ] book gap.
  - [ ] latency spike.
  - [ ] max loss.
  - [ ] max order rate.

Acceptance:

- [ ] paper trading reports hit rate, realized spread, slippage, drawdown, PnL, turnover.
- [ ] kill switches trigger deterministically in replay.

## Deferred Work

Do not prioritize these until the market-data lane is correct and measurable:

- generic multi-venue adapter cleanup.
- full execution routing.
- cross-exchange arbitrage.
- model inference in the hot path.
- DPDK or kernel bypass.
- custom JSON parser.
- SBE production integration.
- live order placement.

SBE should enter after JSON baseline and benchmark evidence. It is a parser/transport optimization, not a substitute for book correctness and replay.

## Migration Rules

1. Do not break the existing generic engine while building the fast lane.
2. Do not force the Binance fast lane through generic `MarketEvent` if it adds allocations or clones.
3. Do not move to execution before deterministic replay exists.
4. Do not accept latency claims without p99/p999/max and the exact commit/config/host.
5. Do not benchmark debug builds.

## Immediate Next Tasks

P0 implementation tasks:

- [x] Create `market-core/engine/src/binance_md/` module skeleton.
- [x] Define compact raw frame, depth update, book sync state, feature, and signal structs.
  - 2026-04-28: `CorrectnessBook<N>` now keeps full fixed-point depth and refreshes the fixed-size `TopBook<N>` feature view.
- [x] Implement typed JSON depth parser for `E`, `s`, `U`, `u`, `b`, `a`.
  - 2026-04-28: `parse_depth_update` now uses typed `serde_json::from_slice` into `BinanceDepthUpdate` and normalizes price/qty strings with fixed-point parsing.
- [x] Implement snapshot-bridge state machine with explicit states:
  - `Disconnected`
  - `BufferingDiffs`
  - `FetchingSnapshot`
  - `ApplyingBuffered`
  - `Live`
  - `RebuildRequired`
- [x] Add static tests for sequence bridge and gap detection.
- [x] Add isolated single-message pipeline from raw frame to TopN book, feature, and expiring signal.
- [x] Emit replay records for raw frame, depth update, snapshot, bridge result, feature, and signal payloads.
- [x] Add remote CI job for Binance md crate build/check.
  - 2026-04-28: root Monorepo CI now has a focused `Rust HFT Engine Fast Lane` job running `cargo fmt --check --package hft-engine`, `cargo check -p hft-engine --locked`, and `cargo test -p hft-engine binance_md --locked` on GitHub Actions.

This plan intentionally narrows the next phase. The platform can remain broad, but the next performance-critical work should be narrow.
