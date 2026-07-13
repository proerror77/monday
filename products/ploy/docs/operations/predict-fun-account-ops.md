# Predict.fun Account Operations

Status: guarded operator adapter, packaged on the paused trade host. It is not
an automatic `ployd` execution venue and it never runs on the research host.

The adapter uses the official Predict.fun TypeScript SDK for EIP-712 limit
orders, scoped token approvals, standard Conditional Tokens redemption,
NegRisk redemption, and yield-bearing routes. All write paths are disabled in
the deployed environment unless a protected GitHub workflow enables one exact
operation for one process.

## Wallet custody boundary

Ploy does not create, export, persist, print, rotate, recover, or back up wallet keys. A wallet
custodian or secret manager must inject exactly one signer source on
`ploy-trade-1`:

- `PREDICT_FUN_PRIVATE_KEY` for an ephemeral process environment, or
- `PREDICT_FUN_PRIVATE_KEY_FILE` pointing to a file owned by the process user
  with mode `0600`.

For `PREDICT_FUN_ACCOUNT_TYPE=EOA`, `PLOY_LIVE_ACCOUNT_ID` must equal the
signer's address. For `PREDICT_FUN_ACCOUNT_TYPE=PREDICT_ACCOUNT`, it is the
Predict Account deposit address and the injected key is its exported Privy
signer. The SDK validates that binding before any operation. Do not copy the
signer, API key, or RPC credential to `tango-1-1`.

This is an injected-signer adapter, not a KMS/HSM custody system. The write-gate
environment variables and protected workflow are procedural controls: a root
operator who can read the signer can bypass them. Cryptographically mandatory
human approval requires an external policy-enforcing signer and is outside this
slice. The official JavaScript SDK and ethers keep the private key in JavaScript
strings/objects that cannot be reliably zeroized, so the adapter is deliberately
one-shot and never imported by a daemon.

Required trade-host configuration:

```sh
PLOY_LIVE_ACCOUNT_ID=0x...
PREDICT_FUN_ACCOUNT_TYPE=PREDICT_ACCOUNT
PREDICT_FUN_CHAIN_ID=56
PREDICT_FUN_API_KEY=...
PREDICT_FUN_RPC_URL=https://...
PREDICT_FUN_PRIVATE_KEY_FILE=/run/ploy-secrets/predict-fun-private-key
PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED=false
PLOY_PREDICT_APPROVAL_WRITE_ENABLED=false
PLOY_PREDICT_RECONCILE_WRITE_ENABLED=false
```

Use chain `97` only for the official testnet API. Mainnet is chain `56` and
requires a Predict API key.
The RPC endpoint is part of the trusted custody boundary: it must use HTTPS and
must report the configured chain ID before a signer is constructed.

## Read-only checks and plans

The wallet check constructs the official SDK account and verifies the signer
binding. Order and redemption planning do not authenticate or sign.

```sh
ploy-predict-account-ops wallet check

ploy-predict-account-ops order plan \
  --market-id 123 --token-id 456 --side BUY \
  --quantity 10 --limit-price 0.42 \
  --out /opt/ploy/data/account-ops/predict-order-plan.json

ploy-predict-account-ops redeem check
ploy-predict-account-ops redeem plan \
  --out /opt/ploy/data/account-ops/predict-redeem-plan.json
```

Plans are immutable, content-hashed, bound to account, account type, chain,
release SHA, current venue state, and the exact SDK maker/taker amounts that
will be signed, and expire after ten minutes. Inputs that the SDK would truncate
are rejected during planning. The
output file is created with mode `0600` and must not already exist.

This slice supports expiring LIMIT orders only. It intentionally does not
expose MARKET orders or the REST remove-only endpoint; removing an order from
the book is not an on-chain cancellation. A later automated venue integration
must add official SDK cancellation before it can manage persistent orders.

## Scoped approval and execution

Review the plan and retain the printed SHA-256. First inspect the exact SDK
approval steps:

```sh
ploy-predict-account-ops order approval-check \
  --plan /opt/ploy/data/account-ops/predict-order-plan.json
ploy-predict-account-ops redeem approval-check \
  --plan /opt/ploy/data/account-ops/predict-redeem-plan.json
```

Run `.github/workflows/approve-predict-account-op.yml` from `main` with:

- the exact immutable SHA already deployed to `ploy-trade-1`;
- one operation: `order_approve`, `order_execute`, `order_reconcile`,
  `redeem_approve`, `redeem_execute`, or `redeem_reconcile`;
- the exact plan SHA-256; and
- the exact risk-confirmation phrase.

The protected `ploy-trade-live` environment provides the human approval. The
workflow verifies current `origin/main`, deployed release SHA, plan ownership
and mode, and persistent write-disabled settings. It then enables only the
selected write gate for that one command; it never edits `.env`.

Approvals are derived from the operation, side, NegRisk flag, and yield flag,
but the official SDK implements ERC-20 approval as `MaxUint256` and ERC-1155 as
`setApprovalForAll(true)` for the selected Predict contract. Review that
asset-level authority explicitly; “scoped” means the adapter selects only the
contracts required for this operation, not that allowance is limited to the
order quantity. Execution re-fetches market or
position state, verifies required approvals, and rejects any drift from the
reviewed plan before signing or broadcasting.

## Reconciliation and evidence

Successful redemption requires a status-1 transaction receipt for each item.
The JSONL ledger records operation IDs, order hashes, order IDs, condition IDs,
and transaction hashes without secrets.

If order submission loses its response, the adapter records
`submission_unknown`, retains the write lock, and refuses a retry. Reconcile
the exact operation only after operator review:

```sh
ploy-predict-account-ops order reconcile \
  --plan /opt/ploy/data/account-ops/predict-order-plan.json --sha256 <sha256>
ploy-predict-account-ops redeem reconcile \
  --plan /opt/ploy/data/account-ops/predict-redeem-plan.json --sha256 <sha256>
```

If redemption is failed or ambiguous, keep writes disabled and reconcile the
recorded transaction on-chain. Redemption reconciliation requires both the
public position to disappear and the official SDK's on-chain token balance to
reach zero; API absence alone never clears the lock. A retained Predict lock
also blocks installation of a new trade release. Reconciliation may therefore
use the deployed plan's older SHA after `main` advances, but only when that SHA
is an ancestor of current `main` and still remains the deployed release. Do not
generate or execute a replacement plan until the retained lock and ledger
evidence have been reviewed.
