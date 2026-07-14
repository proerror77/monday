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
- A verified deployment's maximum order notional and slippage are copied into every intent
  envelope. The engine and worker enforce notional again at the final boundary, and the adapter
  consumes slippage for both immediate and limit order paths.
- Fully paginated open-order and recent-fill inspection, collateral balance, and Data API position
  snapshots.
- Authenticated user order/trade events. Startup REST catch-up is buffered until the execution
  worker subscribes, so a fast fill cannot disappear between client connection and stream
  attachment. A private-stream gap emits `ReconciliationRequired`, disables new intents, and
  requires account-wide REST order/trade reconciliation before recovery.
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
- Confirmed taker fills emit an idempotent `FeeCharged` event using the venue fee-rate field;
  maker fills remain fee-free. The fee is reflected in Monday cash and realized PnL.
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
signed deployment envelope, including Paper and Shadow startup. `LiveSmall` is enabled only for a
human-approved Formula artifact targeting a configured Polymarket venue. Other venues and ONNX
artifacts remain fail-closed. The signed envelope must carry finite positive notional, symbol,
order-size, and integer slippage limits; Monday intersects them with the runtime hard limits.

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
4. Move from the zero-limit operator example to a separately reviewed production config with
   explicit non-zero hard position/notional/rate limits. Then sign a human-approved Polymarket
   Formula deployment with stricter non-zero notional, order-size, and slippage limits and a single
   approved token. The zero-limit example itself intentionally cannot be activated for trading.
5. Place a real-money canary only under separate user authorization, then verify private events,
   fee accounting, REST catch-up, cancellation, and final account reconciliation.

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
