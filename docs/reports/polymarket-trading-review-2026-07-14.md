# Polymarket trading review — 2026-07-14

## Outcome

Monday now has a native Polymarket path for public CLOB quotes, authenticated execution, account
inspection, cancellation, safe replacement, private execution events, and REST reconciliation. The
PLOY code remains reference material; it is not a second live-trading authority.

No funded order was submitted during this review.

## Gaps closed

| Surface | Review result |
| --- | --- |
| Instrument identity | Added a stable `POLYMARKET` venue and PredictionMarket outcome-token identity. |
| Public quotes | Added full-book snapshots, zero-size level deletion, last trades, strict token IDs, connection-global reconnect invalidation, and resnapshot gating. |
| Authentication | Added signer-derived EOA, proxy, Safe, and Poly1271 CLOB authentication without storing L2 credentials. |
| Readiness | Added CLOB v2, geoblock, closed-only, Data API, collateral, and V2 allowance checks. |
| Strategy/risk path | Formula emits a legal PredictionMarket intent for a Polymarket book; the generic risk layer now has a dynamic-rule Polymarket VenueSpec instead of dropping the intent. |
| Orders | Added limit, IOC/FAK, FOK, signed-envelope notional/slippage gates, balance/allowance preflight, and no blind retry after ambiguous submit. |
| Management | Added venue-confirmed OMS or exchange-only cancellation, post-cancel authoritative zero-fill verification, non-increasing cancel-confirm-replace, late-fill tombstones, and owner-only, peer-UID-checked Unix IPC commands whose lock/bind/permission failure blocks startup. |
| Account | Added paginated open orders, balances, positions, confirmed recent fills, fee fields, authoritative IPC inspection, and pristine Polymarket bootstrap. |
| Reconciliation | Added venue/account-scoped positions, pre-subscription event buffering, private-stream gap latching, account-wide REST catch-up, bounded fill/fee dedupe, fail-closed unknown activity, and a periodic pause/drain/snapshot barrier. |
| Recovery | Exchange-only startup orders enter a sticky Paused mode with authoritative REST inspection/cancellation but no placement/replacement. A clean latch requires strict catch-up, a second empty order snapshot, and readiness checks; pre-start matched quantity requires state restoration and restart. |
| Activation | Added unsigned no-strategy operator control for inspection/cancellation only; any strategy still needs a signed deployment. Enabled human-approved LiveSmall only for Polymarket Formula deployments with intersected hard limits; other live artifacts/venues stay disabled. |

Important safety decisions:

- Only `CONFIRMED` Polymarket trades are emitted as fills.
- `MATCHED`, `MINED`, and `RETRYING` trades are settlement-pending: private handling and REST
  catch-up keep execution latched until a strict snapshot observes `CONFIRMED` or `FAILED`.
- A private event with the wrong token, side, or unknown order identity pauses execution and requires
  reconciliation.
- Periodic reconciliation disables worker intake and rejects queued intents before reading the
  authoritative account, so an unhealthy snapshot cannot race one more placement.
- A disconnect on the shared public socket invalidates every subscribed Polymarket token book.
- A canceled order retains a 24-hour, 4,096-entry venue-ID tombstone, so a later `CONFIRMED` trade
  cannot escape Monday's order and portfolio lineage. Late fills consume remaining quantity and an
  overfill forces reconciliation.
- Only taker fills are charged. The venue fee-rate field is converted using Polymarket's current
  price-sensitive formula, rounded to five decimals, and deduplicated with the fill.
- A partially filled order is not automatically replaced because cancel/new quantity lineage would
  otherwise diverge from Monday's OMS.
- Replacement cannot increase quantity, cross to a more aggressive price, or run while execution is
  paused/emergency-latched. More aggressive changes must return through Monday's normal risk path.
- A network failure after order submission is treated as an unknown outcome. The adapter and worker
  both stop accepting orders; inspection plus a process restart is required.
- On-chain approve, split, merge, and redeem operations are not included in the execution adapter.
- No IPC command can submit a new order or increase a replacement's reviewed risk.
- `auto_cancel_exchange_only=true` is rejected: only the runtime's restored OMS context may decide
  which authoritative orders can be canceled safely.

## Verification evidence

- `cargo metadata --locked --no-deps --format-version 1`: passed.
- `cargo test --locked -p hft-data-adapter-polymarket`: 5 passed, 1 network smoke ignored.
- `cargo test --locked -p hft-execution-adapter-polymarket`: 23 passed.
- `cargo test --locked -p hft-core -p hft-ports -p hft-strategy-formula`: 69 passed.
- `cargo test --locked -p hft-engine`: 113 passed, 3 documentation examples ignored.
- `cargo test --locked -p hft-oms-core -p hft-portfolio-core`: 30 passed.
- `cargo test --locked -p hft-runtime --features polymarket,infra-ipc --lib`: 62 passed.
- `cargo test --locked -p hft-ipc --features ipc`: 14 passed, including active takeover,
  concurrent stale-socket ownership, and inode-safe cleanup regressions.
- `cargo test --locked -p hft-live --features polymarket --tests`: 28 passed.
- `cargo check --locked -p hft-live --features polymarket`: passed.
- Strict `cargo clippy --all-targets -- -D warnings` passed for the Polymarket execution adapter,
  IPC feature, and runtime IPC feature.
- Public read-only WebSocket smoke using active outcome token
  `112548421964662546558474258688565408276000153279440324883721010878524791926004`:
  received a real full CLOB snapshot.

## Current live boundary

The current network egress is reported by Polymarket as `blocked=true`, country `SG`.
The Monday repository contains no funded Polymarket private key, and the current shell has no
`POLYMARKET_PRIVATE_KEY`, token ID, signature type, or funder configured. Consequently the review
proves the public quote path and no-money execution contracts, but does not claim authenticated
account or funded-order proof. Live activation must be performed from a permitted deployment
jurisdiction with an externally supplied signer, followed by the activation sequence in
`docs/architecture/POLYMARKET_TRADING_INTEGRATION.md`.
