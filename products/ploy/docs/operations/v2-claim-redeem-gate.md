# V2 Claim/Redeem Gate For SDK And Claimer Cleanup

Status: retired claimer replaced by explicit, write-disabled account ops.

This gate still controls Phase 9 SDK slimming. Phase 10 claimer retirement has
been applied after the operator confirmed the account settlement flow has
converted away from the self-claim path.

Operational update on 2026-07-12: Ploy uses the official V2 SDK for data/trading
types and the official Builder Relayer client for manual account operations.
`ploy-account-ops check` is read-only. `plan` emits a ten-minute plan bound to
chain, account, wallet type, release SHA, contract manifest, and content hash.
`execute` additionally requires that exact hash and
`PLOY_ACCOUNT_OPS_WRITE_ENABLED=true`. Deployment always resets the write gate
to `false`; no daemon invokes Redeem automatically. The immutable trade bundle
includes the Node.js runtime used by the official relayer client, so account
operations do not depend on a separately installed host runtime.

The official JavaScript relayer SDK requires string credentials and cannot
provide Rust-style `zeroize` guarantees. Account ops is therefore an isolated,
one-shot process: it reads credentials only at execution/reconciliation time,
never logs them, is not imported by a daemon, and exits after the command.

## Current Dependency Evidence

Read-only checks from the local workspace show why the gate matters:

```sh
cargo tree -p ploy-market-data --features live -i polymarket_client_sdk_v2
cargo tree -p ploy-connectivity -i polymarket_client_sdk_v2
cargo tree -p ploy-market-data --features live -i alloy
cargo metadata --format-version 1 --no-deps | grep '"name":"ploy-claimer"'
```

Current state:

- Active crates use `polymarket_client_sdk_v2 = 0.6.0`; the archived vendored
  V1 SDK is not in an active dependency path.
- The SDK retains its base `alloy` types; only its `ctf` feature enables the
  contract/provider subfeatures. Ploy's Redeem path remains outside a daemon.
- Before retirement, `ploy-claimer` pulled both `alloy` and `ethers-core` /
  `ethers-signers`.
- After retirement, `ploy-claimer` should not appear in workspace metadata or
  any `ploy-strategy-runtime` feature graph.

## Required Post-V2 Evidence

Capture this evidence before enabling account-ops writes:

1. Pick a resolved V2 market where the account held a winning position.
2. Record the market id, condition id, token id, settlement timestamp, and
   account address.
3. Check the Polymarket Data API position state:
   - `redeemable`
   - payout amount
   - whether the position disappears or becomes paid without a manual redeem
4. Check wallet balance movement for settlement proceeds.
5. If no auto-credit happens, test the gasless relayer path first.
6. Review the generated plan and exact SHA, then enable writes for one command.
7. Save the relayer transaction id, tx hash, adapter route, JSONL ledger event,
   and pUSD before/after balances.
8. Confirm the Data API no longer reports the condition as redeemable. Any
   submitted or ambiguous operation must be reconciled, never resubmitted.

## Decision Table

| Evidence | Phase 9 action | Phase 10 action |
| --- | --- | --- |
| V2 auto-redeems or equivalent settlement credit is verified | Keep SDK signing features isolated | `ploy-claimer` retired |
| V2 still requires manual redeem and relayer works | Keep account ops write-disabled by default | Use reviewed plan + official relayer |
| V2 still requires manual redeem and relayer is unavailable | Block manual Redeem | Repair official relayer access; do not improvise a signer path |

## Retired Claimer Guardrails

- Do not reintroduce `ploy-claimer` as a default/live dependency.
- Do not reintroduce `ethers-core` or `ethers-signers`.
- If manual redeem becomes necessary again, add a new account-ops capability
  instead of restoring the in-process live-runner daemon.
- Route pUSD CTF actions through the current standard or NegRisk collateral
  adapter manifest. Direct legacy USDC.e CTF calls are forbidden.

## Operator Flow

```sh
ploy-account-ops check
ploy-account-ops plan --out /root/redeem-plan.json
# Review the plan, then use its printed SHA exactly once:
PLOY_ACCOUNT_OPS_WRITE_ENABLED=true \
  ploy-account-ops execute --plan /root/redeem-plan.json --sha256 <sha256>
# If execute reports an ambiguous or still-redeemable transaction, keep writes
# disabled and reconcile that exact relayer transaction before any new plan:
ploy-account-ops reconcile --transaction-id <relayer-transaction-id>
# If the submission response was lost before a transaction ID was returned:
ploy-account-ops reconcile --operation-id <ledger-operation-id>
```

## Gate Verification Commands

```sh
scripts/check_v2_claim_redeem_gate.sh
```

The script records the current SDK dependency state and confirms whether
`ploy-claimer` remains retired from the workspace.
