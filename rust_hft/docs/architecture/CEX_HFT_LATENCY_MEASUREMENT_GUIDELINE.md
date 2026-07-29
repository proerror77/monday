# CEX HFT Latency Measurement Guideline

This guideline defines what this repository may call a latency improvement, what each timestamp means,
and what evidence is required before a latency result can affect production readiness. It applies
to public market data, local strategy and risk processing, order submission, venue acknowledgements,
and private execution reports.

Audit base: `9d7b2cca23b05588ed5608afc9926ad29cecd5b4`.

## Non-negotiable rule

Never compare before/after cohorts whose producer, clock, stream, endpoint, or capture boundary
differs, and never subtract timestamps from incompatible clock domains. A field named `timestamp`,
`received`, `submission`, or `ack` is not evidence of its boundary; the code that records it defines
the boundary.

Public trades are features and accounting evidence. They are not the reference clock for order-book
transport latency or the authority for the state of our own orders.

## Canonical timestamps

| Name | Required meaning | Clock | Permitted use |
| --- | --- | --- | --- |
| `exchange_event_ts` (`E`) | Venue event timestamp carried by that message | Venue wall clock | Same-message-family venue-to-local estimate |
| `exchange_trade_ts` (`T`) | Venue match/trade timestamp | Venue wall clock | Trade event time, features, and `T -> E` analysis; not tie-breaking sequence |
| `userspace_frame_ts` | Complete WS message delivered by the WS library to the application | Local monotonic | Start of userspace parse and processing spans |
| `userspace_wall_frame_ts` | Disciplined local wall-clock sample paired with `userspace_frame_ts` at the same boundary | Local wall clock | Cross-clock venue-to-local estimate only |
| `kernel_rx_ts` | Kernel or hardware RX timestamp with documented mechanism | Host/kernel clock | Network attribution only when actually captured |
| `parse_done_ts` | Message validation and decode complete | Local monotonic | Parse latency |
| `queue_publish_ts` | Parsed event accepted by the bounded adapter queue | Local monotonic | Producer-side queue admission |
| `book_commit_ts` | Validated update committed to the canonical book/BBO | Local monotonic | Queue plus book-processing latency |
| `intent_ts` | Strategy emits a complete order intent | Local monotonic | Start of execution hot path |
| `risk_done_ts` | Independent risk/lifecycle gate accepts the intent | Local monotonic | Strategy-to-risk and risk duration |
| `write_start_ts` | First userspace transport write attempt | Local monotonic | Local admission-to-write latency |
| `write_return_ts` | Configured transport API returns success after the complete request buffer has been written | Local monotonic | Whole-request userspace write duration; not proof of kernel or NIC transmit |
| `kernel_tx_ts` | Kernel or hardware TX timestamp with documented mechanism | Host/kernel clock | NIC/network claim only when actually captured |
| `request_response_ts` | Execution-client call returns a decoded synchronous venue response | Local monotonic | Write-to-response latency only |
| `private_order_ack_ts` | Private order stream reports venue acceptance locally | Local monotonic | Authoritative write-to-private-ACK latency |
| `private_report_ts` | Private order/fill event received locally | Local monotonic | Authoritative own-order state and report latency |

Rules:

- Preserve `E`, `T`, and local receive timestamps separately. Never overload one canonical field
  with different meanings for different event variants.
- Sequence trades with the venue's trade ID or sequence identifier. Use `T` as event time only;
  equal timestamps do not define a deterministic order.
- Use a monotonic clock for all local duration measurements. Use a disciplined wall clock only for
  venue-to-local estimates and report its measured offset/error bound.
- Do not name a userspace timestamp `kernel`, `socket RX`, `epoll wake`, or `NIC TX` unless the
  corresponding kernel or hardware capture mechanism exists and is recorded in the artifact.
- A userspace async write returning successfully does not prove that the NIC transmitted the packet.
- If the transport can perform partial writes, `write_return_ts` is recorded only after the final byte
  of the request is accepted. A first-write timestamp may be retained separately but cannot start
  response or private-report spans that claim the whole request was emitted.

## What each public-feed measurement means

For the same message family:

```text
venue_to_userspace_E = userspace_wall_frame_ts - E
trade_to_userspace_T = userspace_wall_frame_ts - T
trade_internal_gap   = E - T
parse                = parse_done_ts - userspace_frame_ts
queue_to_book        = book_commit_ts - queue_publish_ts
userspace_to_book    = book_commit_ts - userspace_frame_ts
```

`venue_to_userspace_E` is not pure network latency. It excludes the visible `T -> E` interval when
both fields exist, but may still contain venue work after `E`, batching, gateway serialization,
network transport, WS/TLS processing, scheduler delay, and local clock error.

`trade_to_userspace_T` additionally contains the venue's match-to-publication path. It must not be
used to accept or reject an order-book transport optimization. The only valid before/after test uses
the same order-book stream, endpoint, named host, clock discipline, and local capture boundary.

Order-book quantity reduction may be a cancel or a trade. A later public trade can classify or
reconcile the observation, but private order reports remain authoritative for our own order state.

## Required benchmark protocol

Every benchmark artifact must include:

- Git SHA, build profile, feature flags, host identity, instance type, AZ, CPU affinity, NIC/IRQ
  settings, endpoint, protocol, stream name, symbol cohort, and UTC start/end times.
- Timestamp producer and exact capture location for every reported span.
- Clock source plus measured offset, maximum error, and synchronization status. Samples observed
  while clock health is outside the declared bound are invalid for cross-clock results.
- Warm-up duration, eligible sample count, reconnect count, sequence gaps, queue overflows, parser
  failures, and dropped/excluded sample counts with reasons.
- p50, p95, p99, p99.9, maximum, and sample count for every reported cohort. p50 is the baseline;
  tails are the readiness evidence. A p99.9 value with fewer than 10,000 eligible samples must be
  marked `insufficient-sample`, not used as a gate.
- Before/after distributions from comparable time windows or an A/B design. A public Trade series,
  another endpoint, or another capture boundary is not a control group.

Do not set a universal microsecond SLO from an article or a single run. Establish venue-, endpoint-,
and host-specific baselines first, then define regression budgets against measured distributions.

## Execution evidence

Report these spans separately; never collapse them into one `submission` number:

```text
intent -> risk_done
risk_done -> write_start
write_start -> write_return
write_return -> request_response
write_return -> private_order_ack
write_return -> private_report
intent -> private_report
```

Order acceptance, synchronous response, private acknowledgement, partial fill, cancel acceptance,
and terminal cancellation are different states and must retain distinct event types.

A claim such as "intent to NIC is below 30 microseconds" requires `kernel_tx_ts` or equivalent
packet-level proof plus a paired-clock mapping that converts `intent_ts` and `kernel_tx_ts` into one
clock domain with a measured conversion error bound. Without that mapping, report
`intent -> write_return` and kernel transmit timing separately; do not subtract them or rename the
userspace span as NIC transmit latency.

## Outcome validation

Lower local latency is necessary but not sufficient. A production comparison also reports:

- Cancel success under stressed market-data load, stratified by venue, symbol, order age, and
  volatility/load regime. Define success as a private terminal cancel before any conflicting fill;
  request acceptance alone is not success.
- Passive-fill markout at declared horizons, stratified by venue, symbol, side, volatility regime,
  quote age, and fill type. Publish the sign convention and price source.
- Unknown-order outcomes, reconciliation invocations, feed invalidations, queue overflows, and
  fail-closed time. Tail improvements obtained by dropping or excluding bad samples are invalid.

Public trades can support aggressor-side and volume features and can classify earlier book changes.
They do not replace the private report stream for fills or the comparable order-book stream for
latency regression evidence.

## Evidence levels

Keep these claims separate:

1. **Repository evidence:** code paths, tests, and local benchmark artifacts exist at an exact SHA.
2. **Exchange-wire evidence:** a controlled host captured comparable live public/private messages.
3. **Deployment evidence:** the exact SHA and configuration are running on the named hosts.
4. **Outcome evidence:** cancel success, markout, reconciliation, and fail-closed behavior improved.

Passing one level does not prove the next. No guideline or local test enables live trading.

## Audit snapshot at `9d7b2cca`

The following findings were true at the audit base. This is an immutable evidence snapshot; live
status belongs to the linked issues. Each runtime change needs its own counterexample test, rollout
unit, and rollback boundary.

| Priority | Finding | Required correction | Issue |
| --- | --- | --- | --- |
| P0 | `MarketEvent` variants overload `timestamp`: Binance depth uses `E`, trade uses `T`, and Spot `bookTicker` uses local adapter time. | Preserve typed `E`, `T`, and local receive fields; prevent cross-variant subtraction from compiling or passing validation. | #395 |
| P0 | `MetricsRegistry::update_from_latency_monitor` observes each rolling mean `count` times, producing a histogram of duplicated means instead of raw samples. | Export raw histogram state or record each sample once; add a counterexample with a deliberately skewed distribution. | #397 |
| P0 | `Submission` measures the full `place_order_envelope().await` call. No canonical `write_start`, `write_return`, or TX timestamp exists. | Instrument the execution boundary and response/private-report paths separately before enforcing a local-write SLO. | #399 |
| P0 | `OrderIntentLifecycle::created_ts` is populated from the triggering market event's local receive time, not a strategy intent-emission timestamp. | Add a separate monotonic `intent_ts` for measurement while retaining wall-clock lifecycle `created_ts`/`valid_until`; migrate all lifecycle operands and validation clocks atomically if those fields ever change domains. | #399 |
| P1 | `WsFrameMetrics.received_at_us` is recorded after Tungstenite yields a complete message, while comments/metric help describe epoll or receive latency. | Rename/document it as userspace frame delivery, or implement real kernel/hardware timestamps before making a lower-layer claim. | #396 |
| P1 | `LatencyStageStats` and `LatencyMonitor` report only through p99; the default rolling window is 200 samples. | Add evidence-grade p99.9 storage/export with sufficient retention and explicit sample sufficiency. Keep the small health window only if it is labeled operational, not benchmark evidence. | #398 |
| P1 | Default `MarketStream::subscribe_tracked` starts tracking at the adapter publish boundary, and snapshot events can use that fallback. | Attach capture provenance to tracked events and reject mixed-boundary benchmark cohorts. | #396 |
| P1 | The optimized `binance-md` app records receive time after `ws.next()` and message-to-bytes conversion; its depth pipeline ends at signal generation. | Label the actual userspace boundary and extend only the missing spans required by a separately scoped benchmark issue. | #396, #399 |
| P2 | Prometheus latency buckets top out at 10 ms for generic stages and current metric names do not encode capture provenance. | Define venue/stage-appropriate buckets and provenance labels without unbounded cardinality after the timestamp contract is fixed. | #398 |

## Code anchors

All paths below are relative to the repository's `rust_hft/` directory.

- Generic tracking boundary: `market-core/ports/src/traits.rs`, `MarketStream::subscribe_tracked`.
- Timestamp and percentile model: `market-core/core/src/latency.rs`, `LatencyTracker`,
  `LatencyStageStats`.
- Operational rolling window: `market-core/engine/src/latency_monitor.rs`, `LatencyMonitorConfig`,
  `LatencyMonitor::get_stage_stats`.
- WS userspace capture: `market-core/integration/src/ws.rs`,
  `WebSocketClient::receive_message_bytes`; and `market-core/integration/src/latency.rs`,
  `WsFrameMetrics`.
- Binance event semantics: `data-pipelines/adapters/adapter-binance/src/message_types.rs` and
  `converter.rs`, `convert_depth_update`, `convert_trade_event`, `convert_book_ticker_event`.
- Binance optimized lane: `apps/binance-md/src/main.rs` and
  `market-core/engine/src/binance_md/pipeline.rs`.
- Collector receive boundary: `tools/collector/src/bin/binance-lob-archiver.rs`; the timestamp is
  recorded after the WS library returns a text message and before JSON parsing.
- Execution span: `market-core/engine/src/execution_worker.rs`,
  `ExecutionWorker::process_order_intents`.
- Intent lifecycle origin: `market-core/engine/src/lib.rs`, the strategy-to-execution queueing flow
  that calls `OrderIntentLifecycle::new` with `created_ts`.
- Prometheus export and buckets: `infra-services/core/metrics/src/lib.rs`,
  `MetricsRegistry::create_with_prometheus` and `MetricsRegistry::update_from_latency_monitor`.

## Review checklist

A latency claim is reviewable only when all answers are yes:

- Are timestamp meanings and capture sites explicit?
- Are local spans monotonic and cross-clock error bounds reported?
- Is the comparison the same stream, endpoint, named host, and cohort?
- Are p50/p95/p99/p99.9, count, exclusions, gaps, and overflows present?
- Is p99.9 sample sufficiency met?
- Are public market data, synchronous responses, and private reports kept distinct?
- Does the claim stop at its evidence level?
- Are stressed cancel success and stratified markout reported before promotion?
- Does every unknown or stale state remain fail-closed?
