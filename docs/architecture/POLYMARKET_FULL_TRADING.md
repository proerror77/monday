# Polymarket Full Trading Integration

## Outcome

Monday owns the complete Polymarket trading lifecycle while PLOY remains a
product, research, and operator surface. The supported lifecycle is:

- discover a market and bind every outcome token to a stable instrument;
- stream public books, best bid/offer, trades, and recovery state;
- quote, place, cancel, replace, and reconcile CLOB orders;
- consume private order and fill events with REST catch-up after gaps;
- hydrate and reconcile collateral, conditional-token positions, open orders,
  fills, and fees per account;
- plan, approve, split, merge, redeem, submit, and reconcile on-chain account
  operations through the official Polymarket relayer flow;
- accept authenticated typed PLOY intents only through Monday's risk, OMS,
  execution, and reconciliation authority.

This change does not enable live trading. Live execution remains fail-closed
until credentials are injected outside source control and the read-only
readiness and reconciliation gates pass for the configured account.

## Authority Boundary

```text
PLOY typed intent
        |
        v
authenticated Monday control API
        |
        v
risk -> OMS -> execution worker -> Polymarket adapter
        |                         |
        |                         +-> CLOB order/private stream
        +-> portfolio/account truth
                                  +-> account-operation runner -> relayer/CTF
```

PLOY must never construct or call a write-capable venue client. Monday is the
only component allowed to hold an execution client or invoke an account
operation. Research and sidecar code cannot resume a runtime, change risk
limits, or broadcast a transaction.

## Stable Identity

Every Polymarket instrument must retain:

- CLOB token ID;
- condition ID;
- outcome name and outcome index;
- standard or negative-risk market family;
- collateral token and adapter identity;
- tick size, lot size, and venue-supplied fee parameters.

The token ID is the execution symbol. Condition and outcome identity are kept
as structured metadata so positions and redemption cannot be inferred from a
display symbol.

Monday's `client_order_id` is the idempotency key. The adapter fixes all signed
fields before signing, derives a deterministic order salt from the full stable
ID, and retains the full ID in adapter metadata. An ambiguous submission is
reported as reconciliation-required and is never blindly resubmitted.

## Runtime Gates

Connection readiness requires all of the following:

1. Polygon and Polymarket endpoints pass validation and geoblock is clear.
2. The signer, signature type, and funder/deposit wallet agree.
3. CLOB v2 credentials are usable or can be derived.
4. pUSD collateral and required exchange/adapter allowances are readable.
5. Public and private streams are ready.
6. A complete authoritative account snapshot is available.
7. Local OMS orders, collateral, positions, and venue state reconcile per
   account without aggregation across accounts.

Any unknown token, missing valuation, pagination failure, stream gap, ambiguous
transaction, or account mismatch keeps intake paused.

## Account Operations

Account operations use a content-addressed plan and append-only ledger. Plans
support `approve`, `split`, `merge`, and `redeem`, bind the account, market
family, condition, token amounts, release identity, and expiry, and acquire an
exclusive lock before submission. Submission outcomes are `confirmed`,
`failed`, or `unknown`; `unknown` must be resolved by operation/transaction
reconciliation before another submission is allowed.

The official JavaScript relayer and signing packages are retained behind a
Monday-owned tool/control surface because the Rust SDK does not yet model the
complete pUSD collateral-adapter and relayer flow. Private material remains in
environment/secret inputs and must not be written to plans or logs.

## Acceptance

- Public snapshot/delta/trade conversion and reconnect recovery are tested.
- CLOB authentication, order mapping, idempotency, cancellation, replacement,
  private-stream catch-up, pagination, fees, and ambiguous outcomes are tested.
- Startup hydration and per-account collateral, position, order, and fill
  reconciliation are tested.
- Approval, split, merge, redeem, plan hashing, locking, relayer submission,
  and unknown-outcome reconciliation are tested without broadcasting.
- Runtime feature registration creates exactly one Polymarket market-data and
  execution path.
- Authenticated PLOY intents enter the same Monday execution queues as native
  runtime intents and cannot bypass risk or OMS.
- Live execution stays disabled by default, and all repository validation uses
  mocks/read-only probes only.
