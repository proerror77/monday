# Market Data Hot Path

This contract separates quote latency, depth coverage, and recovery. A public exchange feed
cannot guarantee that a TCP/WebSocket connection will never drop. The runtime guarantee is
therefore **no stale trading**, not impossible network availability.

## Runtime Flow

```text
Binance: WebSocket L20 snapshots (100 ms) + real-time bookTicker + raw trades
Bybit:   WebSocket L1 snapshots (10 ms) + L50 snapshot/deltas (20 ms) + raw trades
                         |
                         v
bounded adapter queue -> sequence-aware canonical L2 -> real-time BBO overlay
                         |
                         v
strategy context -> executable-price enrichment -> risk -> lifecycle gate -> execution
```

The BBO overlay never mutates canonical L2. When a newer quote moves the best price, published
Top-N filters depth levels that contradict that quote and retains valid deeper levels. A newer
depth image supersedes the overlay.

## Modes

### Binance default hot mode

- `BINANCE_DEPTH_MODE=partial20` (default): WebSocket-only L20 snapshots at 100 ms.
- The default public endpoint is `wss://data-stream.binance.vision/ws`, Binance's market-data-only
  endpoint. Runtime `ws_public` may override it after a deployment-region latency benchmark.
- `BINANCE_SUB_BOOK_TICKER=true` (default): real-time best bid/ask updates.
- `BINANCE_SUB_KLINE=false` (default): bars are derived from raw trades, avoiding a duplicate
  feed on the hot connection.
- `BINANCE_EVENT_QUEUE_CAPACITY=4096` and `BINANCE_SYNC_BUFFER_CAPACITY=16384` are bounded.
  Publishing is non-blocking: overflow invalidates the whole queued generation and reconnects
  instead of allowing the socket reader to build a stale backlog.
- `MarketStream::connect()` does not issue a REST ping.

This mode spends no public REST request weight during normal startup or recovery.

### Binance deep reconstruction mode

- `BINANCE_DEPTH_MODE=diff`: diff-depth at 100 ms with the exchange-required REST snapshot bridge.
- `BINANCE_SNAPSHOT_DEPTH=5000` by default; valid configured tiers are 100/500/1000/5000.
- The WebSocket reconnects immediately. Only the REST snapshot request has a 60-second cooldown;
  the reconnected socket is continuously drained while that budget cools down.
- During the actual snapshot fetch, only depth deltas are buffered. Pre-sync trades and quotes are
  discarded so recovery delay cannot be presented to strategy lifecycle checks as fresh flow.

Use this mode only when deeper-than-L20 reconstruction is required and account for REST IP weight.
Do not run repeated deep snapshot recovery as the latency lane.

### Bybit default hot mode

- The default subscribes to L1 snapshots at 10 ms for `TopOfBook` and L50 snapshot/deltas at 20 ms
  for canonical depth. L1 never mutates L2 directly, and no REST snapshot is required.
- `BYBIT_DEPTH_LEVELS=50` selects the canonical depth tier. Setting it to `1` preserves the legacy
  L1-only snapshot mode; 50/200/1000 also receive the dedicated L1 quote stream.
- Spot subscriptions are split into at most 10 args per request, including L1, depth, and trade
  topics, so multi-symbol startup stays inside Bybit's documented request limit.
- `BYBIT_EVENT_QUEUE_CAPACITY=4096` is bounded and non-blocking. Overflow increments the feed
  generation, discards every queued old-generation event, invalidates the venue book, and then
  reconnects for a fresh snapshot.
- The adapter sends the documented application heartbeat every 20 seconds.
- Per-topic update IDs (`u`) are required to increase, but are not assumed to be contiguous because
  the public contract does not make that guarantee. Cross-depth event ordering uses Bybit's `seq`,
  which is the documented field for comparing L1 with L50/L200/L1000.

## Recovery And Risk Invariants

- Protocol Ping/Pong frames are handled inside the WebSocket client and are not disconnects.
- Reconnect uses capped exponential backoff indefinitely; it does not spin or stop after a small
  fixed attempt count.
- A real disconnect, malformed book, provable sequence gap, or bounded-queue overflow invalidates
  the affected venue book before any newer generation is delivered.
- Strategies receive no synchronized L2 context until a fresh snapshot is accepted.
- Exchange timestamps remain on normalized events for causality and research. Runtime latency,
  intent validity, and queue age start from the local runtime-ingestion timestamp; a 20 ms
  exchange batching interval is not misclassified as 20 ms of local processing delay.
- Market orders without an executable venue quote fail closed before risk review.
- Every surviving intent still passes account/position/order-rate risk and the lifecycle gate.
- Order submission is never blindly retried. HTTP 418/429 and Bybit rate-limit codes are known
  rejections; an accepted but undecodable response latches intake for client-order-ID reconciliation.

## Production Boundary

For stronger availability, deploy independent feed processes on separate network paths and fail
over at the normalized event boundary. A single process, host, ISP path, or exchange endpoint is
not a zero-disconnection architecture. REST polling is not a substitute for that redundancy.

Bybit's standard JSON orderbook feed documents monotonic update IDs and cross-depth ordering but no
checksum or continuity guarantee. This adapter fails closed for every locally observable transport,
parse, queue, and ordering fault. Proving that the exchange did not silently omit a delta requires a
second independent feed (or eligible MMWS/Gateway SBE access) and cross-feed book verification; a
single retail WebSocket cannot make that stronger claim.
