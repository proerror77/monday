# Unbounded Channel Audit

Date: 2026-04-29
Scope: `market-core`, `risk-control`, `strategy-framework`, `apps`, `data-pipelines`

Command:

```bash
rg -n "unbounded_channel|UnboundedSender|UnboundedReceiver" \
  market-core risk-control strategy-framework apps data-pipelines -S
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
| `market-core/runtime/src/system_builder/simulated_execution.rs` | simulated execution event stream uses unbounded sender/receiver | must-fix before live/shadow authority | Paper/shadow correctness needs bounded event authority or explicit replay log fallback. Convert to bounded channel or SPSC queue before relying on it for shadow-ready evidence. |
| `data-pipelines/adapters/adapter-bitget/src/bitget_stream.rs` | adapter event sender and mock/test stream channels | must-fix before live MD path | Public adapter output can carry market data. The dedicated `latency_audit` runner is already bounded, but the generic stream surface still needs a bounded policy. |
| `data-pipelines/adapters/adapter-bitget/src/zero_copy_stream.rs` | zero-copy adapter event sender and tests | must-fix before live MD path | Same market-data surface issue as `bitget_stream.rs`; keep test usage isolated or convert production path to bounded. |
| `data-pipelines/adapters/adapter-binance/src/lib.rs` | adapter stream channel | must-fix before live MD path | Generic Binance adapter stream must not be the production low-latency path until bounded or isolated behind a latest-wins policy. |
| `data-pipelines/adapters/adapter-bybit/src/lib.rs` | adapter stream channel | must-fix before live MD path | Same adapter output risk; classify before any live use. |
| `data-pipelines/adapters/adapter-lighter/src/lib.rs` | adapter stream channel | must-fix before live MD path | Same adapter output risk; classify before any live use. |
| `data-pipelines/adapters/adapter-asterdex/src/lib.rs` | adapter stream channel | must-fix before live MD path | Same adapter output risk; classify before any live use. |
| `data-pipelines/adapters/adapter-grvt/src/lib.rs` | adapter stream channel wrapped as stream | must-fix before live MD path | Same adapter output risk; bounded stream conversion needed before production use. |
| `data-pipelines/adapters/adapter-replay/src/lib.rs` | replay adapter channel | test/demo | Replay is offline and not a live latency authority path. Keep deterministic and avoid using as live queue. |

## Current Safe Boundary

The low-latency evidence path does not depend on these generic unbounded adapter
streams:

- Bitget `latency_audit` uses a bounded raw queue and dedicated engine thread.
- Engine execution queues use bounded SPSC queues for `OrderIntent` and
  `ExecutionEvent`.
- Binance fast lane has its own correctness/replay path and should get a Linux
  latency runner before production p99/p999 claims.

## Required Q2 Fixes

- Convert the generic Binance/Bitget live adapter output used by any trading
  runtime to bounded queues or an explicit latest-wins policy.
- Convert `simulated_execution` event stream before using it as shadow-ready
  order-state evidence.
- Add a CI guard that fails on new unclassified `unbounded_channel` usage in
  production paths.
- Keep `ControlCommand` unbounded only while it remains low-rate control-plane
  traffic and never carries market data, order reports, fills, or hot signals.
