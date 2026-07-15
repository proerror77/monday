# Polymarket trading integration

## Authority boundary

Monday is the only live-trading authority. Strategies emit lifecycle-qualified intents; Monday's
risk manager, OMS, execution worker, reconciliation loop, cancellation controls, and emergency
mode remain on the path for every Polymarket order. The imported PLOY workspace supplies protocol
reference code only and cannot place orders for Monday.

The venue uses the official `polymarket_client_sdk_v2` 0.6.0 client. Monday models each outcome as
`AssetClass::PredictionMarket` / `ProductType::PredictionMarket`, with the decimal CLOB outcome
token ID as `Symbol`. A condition ID, Gamma slug, or display label is not an execution symbol.

## Supported surface

- Public CLOB book snapshots, price-level deltas, best bid/ask, and last trades.
- Reconnect invalidation is connection-global: if one token on the shared socket loses continuity,
  every Polymarket book is invalidated and deltas are rejected until each token receives a fresh
  snapshot. Synthetic public trade IDs include all stable fields exposed by the market channel.
- Signer-derived L2 authentication for EOA, proxy, Gnosis Safe, and Poly1271 wallet modes.
- Startup readiness checks for CLOB v2, geoblock, closed-only mode, collateral balance, and both
  standard and negative-risk exchange allowances.
- Limit and immediate orders, venue-confirmed cancellation, and cancel-confirm-replace order
  modification for unfilled orders. Replacement may only reduce quantity or move price away from
  the market (lower for buys, higher for sells); an aggressive repricing or size increase must be a
  new risk-reviewed intent. A partially filled order must be canceled and reconciled first.
- Formula strategies emit a real `PredictionMarket` intent when their authoritative source book is
  Polymarket. A Polymarket-specific `VenueSpec` keeps the generic risk path active while leaving
  dynamic tick/minimum-size enforcement to the adapter's just-in-time CLOB market query.
- A verified deployment's maximum order quantity, capital-derived order notional ceiling, and
  slippage are copied into every intent envelope. The engine and worker enforce both independent
  size ceilings, while the adapter requires them again at the final prepared-order boundary and
  consumes slippage for both immediate and limit order paths.
- Fully paginated open-order and recent-fill inspection, collateral balance, and Data API position
  snapshots.
- Authenticated user order/trade events. Private confirmed fills and startup/gap-recovery REST
  catch-up events use a bounded, single-consumer reliable outbox. Every required batch slot is
  reserved and every batch is staged before fill-dedupe or order state commits; the now-infallible
  sends happen immediately after that in-memory commit. Outbox saturation therefore leaves all
  accounting state untouched. Large catch-ups are chunked without splitting a matching
  `Fill`/`FeeCharged` pair. The worker pulls one adapter event at a time and only its next poll
  acknowledges the prior downstream write, so cancellation or SPSC backpressure replays exactly
  the current unacknowledged event without rewinding an already delivered fill. The engine also
  keeps a bounded 200,000-event Fill/Fee idempotency window before OMS, portfolio, risk, metrics,
  and broadcasts as defense in depth. Stream attachment emits a generation-tagged barrier and appends a matching
  synchronized-state marker behind any catch-up backlog. Older ready events or markers from a
  replaced stream cannot clear or overwrite the latest generation, and a worker that hits its event batch limit defers new
  intents until another drain pass. The worker remains closed after the tail marker until the
  engine acknowledges that every earlier report has been applied to OMS, portfolio, and risk; it
  then rejects every pre-watermark queued intent before reopening. Active gap recovery publishes
  the same generation pair around each REST catch-up and retries a transient worker-side snapshot
  failure without requiring restart. Intake therefore cannot reopen while older fill/fee events
  are merely in transit. This prevents a fast fill from disappearing between client connection,
  task cancellation, and stream attachment. Private-health fault epochs, recovery epochs, and
  their event publication share one transition guard, so an older recovery cannot publish ready
  after a newer fault or replacement stream. A private order/trade that may contain fill state
  queues `ReconciliationRequired` before its first async processing step, disables new intents,
  and requires account-wide REST order/trade reconciliation before recovery. The adapter's own
  placement gate remains closed until the matching engine-applied generation acknowledgement.
  Any failure that occurs before the venue POST is typed as definitely not submitted; only a
  transport failure at the POST boundary can enter sticky unknown-outcome handling.
- A pristine startup that discovers exchange-only orders enters a sticky operator-recovery mode.
  Authenticated REST inspection and cancellation remain available even if the private socket is
  unavailable, while placement and replacement remain disabled. The latch clears only after strict
  account-wide catch-up, a second fully paginated empty-order snapshot, and account-readiness
  checks. Any pre-start matched quantity keeps that process latched until portfolio/OMS state is
  restored and the process is restarted.
- Only `CONFIRMED` trades become fills. `MATCHED`, `MINED`, and `RETRYING` updates do not change
  Monday's account state and explicitly keep intake latched until strict REST reconciliation sees a
  terminal `CONFIRMED` or `FAILED` state. An ambiguous order submission is sticky: account
  inspection and a process restart are required before this adapter will accept another order.
- Canceled/replaced venue aliases remain as 24-hour, 4,096-entry tombstones so a delayed confirmed
  fill is still attributed to its Monday logical order. Late fills continue consuming the
  tombstone's remaining quantity; an overfill forces reconciliation. Fill IDs use a 100,000-entry
  FIFO dedupe window. REST recovery examines account-wide orders and trades; an empty local
  tracking map cannot be treated as a clean account.
- Confirmed taker fills emit an idempotent `FeeCharged` event from the V2 market `fd` schedule using
  `rate × (price × (1 - price))^exponent`. An absent `fd` follows official V2 semantics and is
  treated as zero fee. Metadata request/identity errors, invalid negative rates, and positive
  non-taker-only schedules fail closed; maker fills remain fee-free. The fee is reflected in Monday
  cash and realized PnL. The private stream never waits on fee metadata: a cache miss disables new
  intake until strict REST recovery observes the same final trade and backfills its market schedule.
- A pristine, single-client Polymarket runtime can bootstrap cash and positions from a complete
  authoritative snapshot. Any exchange-open order, incomplete snapshot, non-Polymarket client, or
  existing local OMS/portfolio state prevents bootstrap.
- Periodic authoritative reconciliation is a placement barrier: Monday pauses the engine, disables
  worker intake, rejects already queued intents, and only then reads the venue snapshot. It restores
  the prior Normal/Degraded mode only for a complete healthy report.
- The Polymarket live feature enables the owner-only Unix IPC control surface. Operators can inspect
  authoritative balances/positions/open orders/recent fills, cancel by ID or filter, and perform
  non-increasing replacement. There is deliberately no raw place-order IPC command.

On-chain inventory operations (`approve`, `split`, `merge`, and `redeem`) are intentionally outside
this adapter. They require a separate custody and relayer review.

## Configuration

Start with [polymarket_quotes_only.yaml.example](../../rust_hft/config/dev/polymarket_quotes_only.yaml.example).
It needs only `POLYMARKET_TOKEN_ID`; no wallet secret is read in quotes-only mode.

Live wiring is shown in [polymarket_live.yaml.example](../../rust_hft/config/dev/polymarket_live.yaml.example):

- `secret: ${POLYMARKET_PRIVATE_KEY}` is the Polygon signer key. Keep it in the configured secret
  source, never in YAML, logs, fixtures, or the database.
- The SDK creates or derives the L2 API key/secret/passphrase. Do not copy PLOY CLOB credentials into
  Monday configuration.
- `signature_type` is mandatory for Live mode and has no runtime default. For new API users,
  Polymarket recommends `poly1271` with an explicit non-zero `funder`. Existing EOA, Proxy, and Safe
  accounts must select the signature type matching the account; Proxy and Safe addresses are
  signer-derived, and any configured funder must match that derivation.
- Polymarket `execution_mode` accepts only missing, `Paper`, or `Live` (case-insensitive). Missing or
  Paper mode registers Monday's simulated execution client. `Testnet` remains available to venues
  that implement it, but Polymarket rejects it because the venue has no testnet execution mode.
  `Live` plus `simulate_execution: true` is also rejected so a simulation flag cannot bypass live
  account reconciliation while still constructing a real client.

## Governed activation and operator access

An authenticated no-strategy process may start without a deployment envelope only for account
inspection, reconciliation, and OMS-aware cancellation. It has no place-order IPC method, and every
risk-increasing IPC mutation is denied. As soon as any strategy is configured, `hft-live` requires a
signed deployment envelope, including Paper and Shadow startup. `LiveSmall` remains fail-closed for
Polymarket and every other venue until the repository's real-venue acceptance gate is completed;
human approval alone cannot bypass it. Paper and Shadow envelopes must still carry finite positive
notional, symbol, order-size, and integer slippage limits, which Monday intersects with the runtime
hard limits. The same fields are already enforced by the execution hot path so a later, separately
reviewed live-enablement change does not need to weaken the contract.

The legacy `engine.auto_cancel_exchange_only` option is rejected when set to `true`. A worker does
not own enough restored OMS context to classify exchange-only orders safely after restart; use the
authenticated runtime cancellation commands after authoritative inspection instead.

With the `polymarket` feature, the runtime synchronously binds and secures the Unix socket at
`/tmp/hft_control.sock` with mode `0600` before live startup can succeed. Every accepted connection
must also have the same peer UID as the socket owner before any request is read. A process-lifetime
exclusive lock prevents another Monday process from replacing an active socket; stale cleanup and
shutdown removal are guarded by socket owner, device, and inode. `HFT_IPC_AUTH_TOKEN` can add token
authentication. The typed `IPCClient` exposes:

- `inspect_execution_accounts()` for authoritative open orders, balances, positions, and fills;
- `cancel_order_by_id(...)` for either OMS-tracked or authoritatively discovered open orders, and
  `cancel_orders_filtered(...)` for OMS-scoped batches. Exchange-only cancellation retains the
  authoritative venue/account route, so it does not depend on the configured symbol catalog;
- `replace_order(...)`, subject to OMS identity, pause/emergency, partial-fill, quantity, and price
  checks.

The local `GetAccount`/`GetPositions` commands are not substitutes for
`InspectExecutionAccounts`; they report Monday's in-memory ledger rather than a venue readback.

## Safe activation sequence

1. Run quotes-only with one currently active outcome token and verify snapshot, delta, trade, and
   forced-reconnect recovery.
2. Load the live example with zero risk limits and no strategy, then verify authentication plus
   authoritative IPC account inspection. This is the only unsigned operator-control startup mode;
   any exchange-only order keeps the process Paused but remains cancellable by authoritative ID.
3. Reconcile open orders, balances, positions, and recent fills until the report is complete. If an
   external order already has matched quantity, restore/reconcile Monday's OMS and portfolio state
   and restart instead of trying to clear the recovery latch in place.
4. Under a separate real-venue acceptance task, prepare a reviewed production config with explicit
   non-zero hard position/notional/rate limits and a human-approved Polymarket Formula envelope.
   The current runtime still rejects `LiveSmall`, including this otherwise eligible envelope.
5. Only after that task changes and reviews the fail-closed gate, place a real-money canary under
   separate user authorization and verify private events, fee accounting, REST catch-up,
   cancellation, reduce-only exit, order sizing, slippage, and final account reconciliation.

Building or passing no-money tests does not prove a live canary. Real trading remains disabled by
default and this repository change does not submit a funded order.

## Protocol references

- [Market WebSocket channel](https://docs.polymarket.com/market-data/websocket/market-channel)
- [Authenticated user WebSocket channel](https://docs.polymarket.com/market-data/websocket/user-channel)
- [Authentication and signature types](https://docs.polymarket.com/api-reference/authentication)
- [Order types and placement](https://docs.polymarket.com/trading/orders/overview)
- [Order and trade lifecycle](https://docs.polymarket.com/concepts/order-lifecycle)
- [Current positions API](https://docs.polymarket.com/api-reference/core/get-current-positions-for-a-user)
- [Trade history API](https://docs.polymarket.com/api-reference/trade/get-trades)
- [Trading fees](https://docs.polymarket.com/trading/fees)
