# HFT 2026 Q2 Execution Plan

Date: 2026-04-29
Quarter: 2026 Q2
Status: execution plan

This plan covers the work that can realistically be finished before the end of
2026 Q2. It separates local engineering work from Linux staging work so latency
claims do not depend on macOS scheduler behavior.

The Q2 target is not "full live HFT". The target is:

```text
replayable market data
  -> correct local book / features / signals
  -> explicit signal and order-intent lifecycle
  -> bounded queue topology
  -> paper / shadow evaluation
  -> Linux staging p99/p999 evidence
```

Real-money small live, multi-exchange arbitrage, colocated infra, FIX/SBE order
entry, and kernel-bypass work are intentionally out of Q2 unless all acceptance
gates below are already green.

## Current State

Already done:

- Binance market-data fast lane has parser, snapshot/diff bridge, local book,
  microstructure features, expiring signal, replay writer/reader, live smoke,
  and paper subcommand.
- Bitget market-data adapter matches v2 public WebSocket semantics for
  `books/books1/books5/books15` and `trade`, including text `ping` / `pong`.
- Bitget parser and event-conversion hot path has Criterion benchmarks and live
  p99 probes.
- Bitget `latency_audit` has bounded raw queue, dedicated engine OS thread,
  `--queue-kind sync-channel|spsc-spin`, `--receiver-core`, `--engine-core`,
  `--busy-poll`, `--idle-timeout-us`, `--json-out`, Linux runner, preflight,
  and multi-run summary tooling.
- Engine already has SPSC bounded execution queues for
  `OrderIntent -> ExecutionWorker` and `ExecutionEvent -> Engine`.
- Runtime config already exposes execution queue capacities and worker batch
  settings.
- `OrderIntentEnvelope` now adds lifecycle metadata around the stable
  `OrderIntent` contract, and the execution queue has a pre-execution gate for
  expiry, stale book source, and max local latency.
- Generic Binance stream, Bitget generic `MarketStream`, Bitget zero-copy
  stream, and simulated execution event output are now bounded; new
  unclassified unbounded channel usage is checked by
  `scripts/audit_unbounded_channels.sh`.
- Risk crates already contain default/enhanced/professional managers with
  position, notional, staleness, cooldown, and rate-style checks.

Partially done:

- Queue topology exists in pieces, but the policy contract is not complete for
  every producer/consumer pair.
- Execution event queues are bounded, but full behavior under saturation is not
  yet a production contract.
- Strategies still emit plain `OrderIntent`; generic live/paper paths must now
  adopt `OrderIntentEnvelope` consistently before risk/execution.
- Paper/replay exist, but paper fill modeling is not yet sufficient for queue
  position, cancel delay, and adverse-selection analysis.
- Linux staging scripts exist, but real host/region results have not been
  collected.

Not done:

- Full reconnect lifecycle for Binance/Bitget, including proactive rotation,
  DNS/TLS timing, and cold-path rebuild runbooks.
- Unified overload controller across market data, strategy, risk, execution,
  logging, and metrics.
- Complete OMS state machine evidence for ack/fill/cancel races, duplicate
  reports, unknown orders, and reconciliation recovery.
- Exchange rule manager, cost model, queue-position estimator, and global
  strategy/risk budget manager.

## Q2 Scope

Q2 includes:

- Local code and tests for queue policy, signal lifecycle, risk gates,
  paper/shadow evaluation, and observability contracts.
- Linux staging latency runs for Bitget and Binance market-data paths.
- Documentation and runbooks that make the measured system reproducible.

Q2 excludes:

- Real-money live activation.
- Cross-exchange arbitrage execution.
- DPDK, QAT, FPGA, custom NIC, or kernel bypass.
- Replacing typed JSON with custom parser/SBE before Linux staging identifies
  parser cost as the dominant p99/p999 bottleneck.

## Workstream A: Market-Data Correctness And Replay

Status: mostly complete for Binance; partial for Bitget.

Tasks:

- [x] Binance snapshot + buffered diff bridge tests.
- [x] Binance gap -> rebuild-required behavior.
- [x] Binance feature/signal replay records.
- [x] Binance replay reader and parity path.
- [x] Bitget typed borrowed parsers and live p99 probes.
- [ ] Add explicit replay schema versioning.
- [ ] Add Bitget event replay shape for `books` / `books1` / `trade`.
- [ ] Add reconnect lifecycle checklist tests where behavior can be simulated.
- [ ] Add sanity-check docs for bookTicker / top-of-book comparison.

Acceptance:

- `cargo test -p hft-engine --locked binance_md`
- `cargo test -p hft-data-adapter-bitget --locked -- --test-threads=1`
- Replay parity proves same input creates same final book, features, and
  signals.

## Workstream B: Queue Topology And Backpressure

Status: partially implemented.

Target topology:

```text
Market data path:
  WebSocket receiver -> raw_market_data_queue -> market_data_engine

Fast signal path:
  market_data_engine -> signal_queue -> strategy/risk boundary

Execution path:
  risk-approved OrderIntent -> execution_intent_queue -> execution_worker

Feedback path:
  exchange/user stream -> execution_event_queue -> order_state/position/risk

Side paths:
  trace_queue -> metrics aggregator
  log_queue -> logger
```

Queue policy:

| Queue | Must be bounded | Full behavior | Notes |
| --- | --- | --- | --- |
| `raw_market_data_queue` | yes | latest-wins or rebuild, never block engine | stale old data is worse than missing data |
| `signal_queue` | yes | drop expired / drop old | signals must carry expiry |
| `execution_intent_queue` | yes | reject/throttle new opens, allow reduce-only if configured | no silent order loss |
| `execution_event_queue` | yes | no silent drop; alert/degrade if full | fills/acks are state authority |
| `risk_event_queue` | yes | never drop kill-switch events | control-plane priority |
| `log_queue` | yes | drop debug/info when full | logging must not backpressure trading |
| `metrics_queue` | yes | sample/drop when full | metrics must not block hot path |

Tasks:

- [x] Add a queue topology contract document with owner, capacity, full behavior,
  and test command for each queue.
- [x] Add tests for execution intent queue full behavior.
- [x] Add tests for execution event queue full behavior and alert counters.
- [x] Audit `unbounded_channel` usages and classify each as test/demo,
  control-plane acceptable, or must-fix.
- [ ] Convert remaining hot-path or feedback-path unbounded channel findings
  from the audit.
- [ ] Add a queue-summary output that surfaces capacity, utilization, full count,
  and dropped count per active queue.
- [x] Add a guard script for new unclassified `unbounded_channel` usage.

Acceptance:

- No unclassified `unbounded_channel` remains in production paths.
- Every queue has an explicit full behavior.
- Execution events are not silently lost in normal operation.

## Workstream C: Signal And OrderIntent Lifecycle

Status: Binance fast lane has expiring signal; generic order path is incomplete.

Tasks:

- [x] Define a lifecycle wrapper for strategy output, preserving compatibility
  with current `ports::OrderIntent`.
- [x] Add `created_ts`, `valid_until`, `source_book_seq`, `source_feature_ts`,
  `max_latency_us`, `max_slippage_bps`, and `reduce_only` where appropriate.
- [x] Add a pre-risk expiry gate.
- [x] Add a pre-execution expiry gate.
- [x] Add tests proving expired intents never enter the execution queue.
- [x] Add tests proving stale book/source sequence invalidates a signal.

Acceptance:

- Strategies can still emit `OrderIntent`, but live/paper paths use a lifecycle
  envelope before risk/execution.
- Expired or stale signals are rejected with metrics.
- No strategy can mutate orders, positions, or balances directly.

## Workstream D: Risk, OMS, And Reconciliation Correctness

Status: risk managers and execution worker exist; OMS correctness still needs
focused tests.

Tasks:

- [ ] Create an order-state-machine test matrix for:
  `pending_new`, `acknowledged`, `partially_filled`, `filled`,
  `pending_cancel`, `cancelled`, `cancel_rejected`, `rejected`, `expired`,
  and `unknown`.
- [ ] Add tests for fill-before-ack.
- [ ] Add tests for duplicate ack/fill reports.
- [ ] Add tests for cancel/fill race.
- [ ] Add tests for ack timeout -> cancel path.
- [ ] Add reconciliation dry-run tests for exchange-only orders and local-only
  orders.
- [ ] Add position mismatch handling rules: stop opening, reconcile, then resume
  only after state is known.

Acceptance:

- OrderManager owns order state through one writer.
- Risk/portfolio owns position/exposure through one writer.
- Unknown state leads to stop-opening or shutdown, not continued trading.

## Workstream E: Paper And Shadow Evaluation

Status: basic paper path exists; fill realism is incomplete.

Tasks:

- [ ] Add event-level paper fills for market orders.
- [ ] Add limit fill simulation with queue-position approximation.
- [ ] Add partial fill simulation.
- [ ] Add fees, spread cost, slippage, cancel delay, and latency penalty.
- [ ] Add signal-to-intent evaluation with expiry respected.
- [ ] Add shadow-live mode: generate orders and metrics without sending orders.
- [ ] Report hit rate, realized spread, slippage, turnover, PnL, drawdown,
  and expected edge vs realized edge.

Acceptance:

- Same replay input produces deterministic paper results.
- Paper evaluation can reject strategies whose edge is below cost + safety
  margin.

## Workstream F: Linux Staging Latency Evidence

Status: tooling complete; real host results pending.

Tasks:

- [x] Add Linux preflight.
- [x] Add Bitget Linux runner with `taskset`, receiver core, engine core,
  queue kind, busy-poll, metadata, stdout, and JSON summary artifacts.
- [x] Add summary script for multi-run comparison, including queue kind and
  idle timeout metadata.
- [ ] Run Bitget 5-minute staging audit in at least one Linux region.
- [ ] Run Bitget multi-run comparison for at least 3 runs on the same host.
- [ ] Run Binance equivalent staging audit or add the missing parity runner.
- [ ] Compare Tokyo / Singapore / Hong Kong / Frankfurt if hosts are available.
- [ ] Record p50/p95/p99/p999/max with commit SHA, host, kernel, CPU, governor,
  RUSTFLAGS, receiver core, engine core, and busy-poll mode.

Acceptance:

- `summary.json` artifacts exist for every claim.
- `scripts/summarize_bitget_latency.py target/latency-audit` produces the
  comparison table.
- p99/p999 conclusions use Linux staging data, not macOS data.

## Workstream G: Overload And Safety Modes

Status: checklist exists; code-level controller incomplete.

Overload levels:

| Level | Trigger | Behavior |
| --- | --- | --- |
| 0 | normal | full market data, features, signals, paper/shadow |
| 1 | metrics/log pressure | downsample metrics, drop debug logs |
| 2 | queue pressure | pause slow strategy, keep book/features |
| 3 | signal stale/latency spike | stop new signals, keep book current |
| 4 | execution/risk unknown | stop opening, reduce-only if configured |
| 5 | severe state unknown | cancel all if safe, shutdown |

Tasks:

- [ ] Define `OverloadLevel` and transition reasons.
- [ ] Connect queue pressure, latency spike, stale book, disconnect, and risk
  alerts to overload transitions.
- [ ] Add tests for degrade and recovery hysteresis.
- [ ] Add operator output showing current level and reason.

Acceptance:

- Overload behavior is deterministic and tested.
- The system never opens new risk when state is unknown.

## Workstream H: Exchange Rules, Rate Limits, And Cost Model

Status: partial abstractions exist; trading-grade checks incomplete.

Tasks:

- [ ] Define per-symbol rule snapshot: tick size, step size, min qty,
  min notional, precision, fee tier, and contract multiplier.
- [ ] Add fast local order validation before execution.
- [ ] Add rate-limit manager for REST weight, order rate, cancel rate, snapshot
  rate, and WebSocket connection rate.
- [ ] Add cost model: maker/taker fee, spread cost, slippage, latency risk,
  adverse selection buffer.
- [ ] Require expected edge > cost + safety margin before order intent can pass
  risk.

Acceptance:

- Precision/rule rejects are caught locally before exchange submission.
- Strategies with no cost-adjusted edge cannot reach execution.

## Q2 Milestones

### Milestone 1: Queue And Lifecycle Contract

Target: early Q2

- Queue topology contract complete.
- Unbounded channel audit complete.
- Signal/order-intent lifecycle design merged.
- Expiry gate tests pass.

Evidence:

- Queue contract document.
- Tests for expired intent rejection.
- Audit output showing every `unbounded_channel` is classified.

### Milestone 2: Trading Correctness Dry Run

Target: mid Q2

- OMS state-machine test matrix complete.
- Ack/fill/cancel race tests pass.
- Reconciliation dry-run tests pass.
- Paper fills include fees, slippage, partial fills, and cancel delay.

Evidence:

- `cargo test -p hft-engine --locked execution`
- `cargo test -p hft-risk --locked`
- `cargo test -p hft-paper --locked`

Adjust command names to actual package names if they differ.

### Milestone 3: Linux Staging Evidence

Target: mid-to-late Q2

- Bitget staging data collected.
- Binance staging data collected or explicitly listed as missing.
- Multi-run summary table generated.
- Kernel/CPU/governor/commit/RUSTFLAGS included for every result.

Evidence:

- `target/latency-audit/<run-id>/summary.json`
- `target/latency-audit/<run-id>/metadata.txt`
- `scripts/summarize_bitget_latency.py target/latency-audit`

### Milestone 4: Shadow-Ready Gate

Target: end of Q2

- Shadow-live mode can run without sending orders.
- Stop-opening and kill-switch behavior is tested.
- Operator can inspect latency, queues, risk, positions, and replay/paper output.

Evidence:

- Shadow run report.
- Replay/paper deterministic report.
- Safety checklist with remaining small-live blockers.

## Deferred Until After Q2

- Real-money small live activation.
- Multi-exchange execution and cross-exchange arbitrage.
- FIX/SBE execution path.
- Custom JSON/SBE parser replacement if Linux p99 shows parser is not dominant.
- DPDK, QAT, kernel bypass, FPGA, colocated infra.

## Definition Of Done For Q2

Q2 is complete when:

- Market-data fast lanes are replayable and measured.
- Queue topology and full behavior are explicit and tested.
- Signals/intents have lifecycle gates.
- Risk/OMS/reconciliation has race-condition test coverage.
- Paper/shadow evaluation can run without live orders.
- Linux staging artifacts support p99/p999 claims.
- Remaining live blockers are listed, not hidden.
