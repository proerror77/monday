# HFT Queue Topology Contract

Date: 2026-04-29
Status: Q2 execution contract

This contract fixes the producer, consumer, capacity owner, and full behavior for
the queues that matter to the low-latency trading path. The rule is simple:
market data may discard stale work, execution authority must not disappear
silently, and logging/metrics must never backpressure trading.

## Topology

```text
Market data:
  WebSocket receiver -> raw_market_data_queue -> market_data_engine

Signal:
  market_data_engine -> signal_queue -> risk/order-intent boundary

Execution:
  risk-approved OrderIntent -> execution_intent_queue -> execution_worker

Feedback:
  exchange/user stream -> execution_event_queue -> engine/order-state/risk

Side paths:
  trace_queue -> metrics aggregator
  log_queue -> logger
  risk_event_queue -> risk/overload controller
```

## Queue Contract

| Queue | Producer | Consumer | Capacity owner | Full behavior | Current code / status | Test command |
| --- | --- | --- | --- | --- | --- | --- |
| `raw_market_data_queue` | WS receiver | market-data engine | adapter/runner config | bounded; latest-wins or rebuild; never block engine | Bitget `latency_audit` and generic Bitget `MarketStream` are bounded; Binance app path still needs the same runner parity | `cargo check -p hft-data-adapter-bitget --example latency_audit --locked` |
| `signal_queue` | market-data engine | strategy/risk boundary | strategy runtime config | bounded; drop expired first, then old signals | Binance fast lane has expiring `Signal`; generic strategy path now has `OrderIntentEnvelope` lifecycle but not a dedicated signal queue | `cargo test -p hft-engine --locked binance_md` |
| `execution_intent_queue` | engine/risk boundary | execution worker | `ExecutionQueueConfig.intent_queue_capacity` | bounded; reject new intent on full, surface counter; no silent order loss | `market-core/engine/src/execution_queues.rs` | `cargo test -p hft-engine --locked test_execution_intent_queue_full_rejects_new_intent` |
| `execution_event_queue` | execution worker / user stream bridge | engine/order state/risk | `ExecutionQueueConfig.event_queue_capacity` | bounded; surface counter and alert/degrade on full; execution authority must be reconciled | `market-core/engine/src/execution_queues.rs` currently returns failed event to caller and counts full events | `cargo test -p hft-engine --locked test_execution_event_queue_full_is_visible_to_stats` |
| `risk_event_queue` | risk/sentinel/reconciliation | risk/overload controller | risk runtime config | bounded; kill-switch events must not be dropped | design contract only; needs implementation with priority for kill switch | pending |
| `trace_queue` | hot path trace writer | metrics aggregator | metrics config | bounded; sample/drop stage detail under pressure, preserve anomaly records | Bitget latency audit records local histograms; generic trace queue remains a Q2 task | pending |
| `log_queue` | app/runtime | logger | logging config | bounded; drop debug/info on full; warn/error may trigger degrade | not yet a central queue contract in code | pending |
| `metrics_queue` | app/runtime | metrics exporter | metrics config | bounded; sample/drop on full; never block hot path | metrics export exists in pieces; queue policy remains a Q2 task | pending |

## Lifecycle Gate

Strategies still emit the stable `ports::OrderIntent`. Live, paper, and shadow
paths should wrap it in `ports::OrderIntentEnvelope` before risk or execution:

```text
OrderIntent
  + created_ts
  + valid_until
  + source_book_seq
  + source_feature_ts
  + max_latency_us
  + max_slippage_bps
  + reduce_only
```

The pre-risk and pre-execution gates reject:

- `Expired`: local time is at or beyond `valid_until`.
- `SourceBookStale`: latest book sequence is newer than the signal source.
- `MaxLatencyExceeded`: local queue/processing delay exceeded the strategy
  budget.

The engine execution queue exposes
`send_lifecycle_intent_with_book_seq(...)`, and tests prove expired/stale
envelopes do not enter the execution worker queue.

## Operating Rules

- Hot-path queues are bounded by default.
- New unbounded channel usage must be classified in
  `docs/reports/UNBOUNDED_CHANNEL_AUDIT.md`; run
  `scripts/audit_unbounded_channels.sh` before merging queue changes.
- Queue-full behavior must be explicit in code, docs, and tests.
- Execution events are state authority; if they cannot be enqueued, the system
  must alert/degrade and reconcile rather than continue as if nothing happened.
- Signal and order-intent queues may drop stale work, but not silently: counters
  must move.
- Logs and metrics are side paths. They can be sampled or dropped; they cannot
  stall market data, risk, or execution.

## Current Q2 Gaps

- Add a Binance receiver -> bounded queue -> engine latency runner equivalent to
  the Bitget runner.
- Convert the remaining production adapter `unbounded_channel` usage classified
  as `must-fix` in the audit.
- Add a real `risk_event_queue` with kill-switch priority.
- Add central log/metrics queue policy and operator output.
