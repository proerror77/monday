# Unbounded Channel Audit

Date: 2026-04-29
Scope: `market-core`, `risk-control`, `strategy-framework`, `apps`, `data-pipelines`

Command:

```bash
rg -n "unbounded_channel|UnboundedSender|UnboundedReceiver" \
  market-core risk-control strategy-framework apps data-pipelines -S
```

Guard command, also wired into the root `Rust HFT Engine Fast Lane` CI job:

```bash
scripts/audit_unbounded_channels.sh
```

Classification:

- `test/demo`: acceptable outside production hot path.
- `control-plane acceptable`: acceptable for commands, lifecycle control, or
  low-rate management paths. Revisit only if it begins carrying market data,
  order acks/fills, or hot strategy traffic.
- `must-fix before live`: production market-data, execution feedback, or trading
  state path that needs a bounded policy before live trading.

## Findings

| File | Usage | Classification | Rationale / next action |
| --- | --- | --- | --- |
| `market-core/engine/src/execution_worker.rs` | `UnboundedReceiver<ControlCommand>` and `unbounded_channel()` for worker control handles | control-plane acceptable | Low-rate worker control path, not order intent or execution feedback. Keep separate from hot path; do not carry market data or reports here. |
| `market-core/runtime/src/system_builder.rs` | stores execution-worker control senders | control-plane acceptable | Sender registry for worker lifecycle/control. Acceptable while it remains command-only. |
| `market-core/runtime/src/system_builder/simulated_execution.rs` | simulated execution event stream | fixed in Q2 local slice | Converted to bounded `tokio::mpsc::channel` with `try_send`, full/closed errors, and a bounded-queue regression test. |
| `data-pipelines/adapters/adapter-bitget/src/bitget_stream.rs` | adapter event sender and stream channel | fixed in Q2 local slice | Converted generic Bitget stream output to bounded `tokio::mpsc::channel` with `try_send` and `BITGET_EVENT_QUEUE_CAPACITY`. Dedicated `latency_audit` remains the p99 evidence path. |
| `data-pipelines/adapters/adapter-bitget/src/zero_copy_stream.rs` | zero-copy adapter event sender and tests | fixed in Q2 local slice | Converted to bounded `tokio::mpsc::channel` with `try_send` and `BITGET_EVENT_QUEUE_CAPACITY`. |
| `data-pipelines/adapters/adapter-binance/src/lib.rs` | adapter stream channel | fixed in Q2 local slice | Converted generic Binance stream output to bounded `tokio::mpsc::channel` with `try_send` and `BINANCE_EVENT_QUEUE_CAPACITY`; initial snapshots fail explicitly if the queue is full. |
| `data-pipelines/adapters/adapter-bybit/src/lib.rs` | adapter stream channel | must-fix before live MD path | Same adapter output risk; classify before any live use. |
| `data-pipelines/adapters/adapter-lighter/src/lib.rs` | adapter stream channel | must-fix before live MD path | Same adapter output risk; classify before any live use. |
| `data-pipelines/adapters/adapter-asterdex/src/lib.rs` | adapter stream channel | must-fix before live MD path | Same adapter output risk; classify before any live use. |
| `data-pipelines/adapters/adapter-grvt/src/lib.rs` | adapter stream channel wrapped as stream | must-fix before live MD path | Same adapter output risk; bounded stream conversion needed before production use. |
| `data-pipelines/adapters/adapter-replay/src/lib.rs` | replay adapter channel | test/demo | Replay is offline and not a live latency authority path. Keep deterministic and avoid using as live queue. |

## Current Safe Boundary

The low-latency evidence path does not depend on these generic unbounded adapter
streams:

- Bitget `latency_audit` uses a bounded raw queue and dedicated engine thread.
- Bitget generic `MarketStream` now uses bounded output as well; full queues
  drop stale market events with a warning instead of growing memory without
  bound.
- Engine execution queues use bounded SPSC queues for `OrderIntent` and
  `ExecutionEvent`.
- Simulated execution now uses bounded event output; paper/shadow code can no
  longer hide unbounded order-report buildup.
- Binance fast lane has its own correctness/replay path; generic Binance stream
  output is now bounded, but the Binance Linux latency runner is still needed
  before production p99/p999 claims.

## Required Q2 Fixes

- Convert the remaining non-Binance/non-Bitget adapter outputs used by any
  trading runtime to bounded queues or an explicit latest-wins policy.
- Keep `scripts/audit_unbounded_channels.sh` in CI so new unclassified
  `unbounded_channel` usage fails before merge.
- Keep `ControlCommand` unbounded only while it remains low-rate control-plane
  traffic and never carries market data, order reports, fills, or hot signals.
