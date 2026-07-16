# Polymarket V2 and Confirmed Settlement Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans task-by-task.

Goal: Move live execution to the official Polymarket CLOB V2 adapter and close the canonical resolution-to-confirmed-redemption lifecycle without synthetic SELL fills or local real-wallet activity.

Architecture: Preserve Ploy's synchronous `LiveExecutionGateway` boundary, replace only the execution adapter with the official SDK, record resolution/redemption as independent trading-domain transitions, and reconcile confirmed redeem activity from a fakeable Data API surface before releasing position or exposure.

Tech Stack: Rust 1.91, Serde, rust_decimal, Tokio, SQLx/PostgreSQL, Polymarket official `polymarket_client_sdk_v2 = "=0.6.0"`, CLOB V2, Data API, and file-mode paper compatibility caches.

## Global Constraints

- Start after the live-execution-safety plan lands because both plans extend `LiveExecutionGateway`.
- Do not sign or submit an order, allowance, split, merge, redeem, or relayer transaction during local work.
- Do not restore `ploy-claimer`, `CLAIMER_*`, `POLY_BUILDER_*`, ethers, or an in-runner redeem daemon.
- PostgreSQL is the canonical production store for deployment/trading/audit state. In PostgreSQL mode, `deployments.json`, `trading-state.json`, and audit JSONL are post-commit recovery/diagnostic caches only and never override a valid database snapshot. File mode remains a local paper-compatibility store and cannot apply or restore a non-archived live deployment.
- Keep domain/runtime crates free of SDK types.
- Preserve payout values `0`, `0.5`, and `1` exactly as `Decimal`.
- Resolution never releases position. Only a confirmed transaction/activity receipt does.
- Keep the legacy vendored SDK available only to the existing market-data integration until that separate migration is justified; exclude its package/examples from workspace test membership.
- Each task is one atomic commit and stages only its owned paths.
- Task 1 alone belongs to branch/PR `feat/polymarket-v2-adapter`. After it merges, Tasks 2-5 run on `feat/polymarket-settlement-lifecycle`; after that merges, Task 6 runs alone on `feat/canonical-live-promotion` from updated `main`.

## Verified Upstream Boundary

Implementation must use the official stable crate and must not track upstream `main`:

```toml
polymarket-client-sdk-v2 = {
  package = "polymarket_client_sdk_v2",
  version = "=0.6.0",
  default-features = false,
  features = ["clob", "data", "gamma", "heartbeats"]
}
```

The official V2 adapter exposes protocol discovery (`version()`), V2 host `https://clob-v2.polymarket.com`, server time, authenticated orders, cancel/cancel-all, geoblock, Data API activity/positions, and optional CTF support. Ploy does not enable the `ctf` feature in the default execution path in this plan.

---

### Task 1: Replace the execution adapter with the official V2 SDK

Files:

- Modify `Cargo.toml`.
- Modify `crates/ploy-connectivity/Cargo.toml`.
- Modify `crates/ploy-connectivity/src/lib.rs`.
- Modify `Cargo.lock`.
- Modify `tools/sdk_auth_check/Cargo.toml` and its Rust source files.
- Add `tests/test_polymarket_v2_execution_contracts.py`.
- Modify `scripts/check_v2_claim_redeem_gate.sh`.

Workspace dependency boundary:

```toml
[workspace]
exclude = ["vendor/polymarket-client-sdk"]
```

Keep `ploy-market-data` on its current optional legacy integration for this PR. The exclusion prevents vendored examples/dev-dependencies such as AWS KMS from becoming workspace test targets; it does not pretend the data adapter has already migrated.

Adapter constants and types:

```rust
const DEFAULT_POLY_CLOB_HOST: &str = "https://clob-v2.polymarket.com";
const DEFAULT_POLY_DATA_HOST: &str = "https://data-api.polymarket.com";
const REQUIRED_CLOB_VERSION: u32 = 2;

pub enum WalletSignatureType {
    Eoa,
    Proxy,
    GnosisSafe,
    Poly1271,
}
```

Keep Ploy's domain trait and request/outcome types. Only `into_sdk()` and adapter internals import `polymarket_client_sdk_v2`.

V2 client initialization rules:

- Construct `Client::new` with the V2 host and current server-time setting.
- Authenticate with the existing signer/funder flow.
- Before signing, normalize the requested token as `U256`, resolve `token -> condition` through CLOB `market_by_token`, and corroborate token membership plus `neg_risk` through CLOB `clob_market_info` and exactly one Gamma market returned for that token. Gamma must provide the same condition ID, contain the requested token in `clob_token_ids`, and contain exactly one non-empty parent event ID. Any disagreement, missing field, duplicate market, or ambiguous event fails before venue mutation.
- Resolve collateral from `polymarket_client_sdk_v2::contract_config(chain_id, neg_risk).collateral`, not from user input. If Gamma supplies `denomination_token`, normalize it as a 20-byte address (reject non-zero upper 96 bits) and require it to equal the SDK contract collateral. The resulting condition, event, token, Binary/NegRisk protocol, and collateral form the safety plan's complete `VenueSettlementIdentity`; cache only this validated immutable identity.
- Call `version().await`; reject any value other than `2` before caching the client.
- `Poly1271` requires an explicit non-zero funder/deposit address.
- Health/readiness calls `check_geoblock()` and rejects `blocked=true`.
- Preserve FAK/FOK/GTC mapping and partial-fill receipts exactly.
- Every acknowledged V2 outcome carries `settlement_identity=Some(validated_identity)`; reconciliation later copies this canonical identity and never derives it from closed-position/activity DTOs.
- Preserve paginated order/trade reconciliation, cancel-all, open-order listing, and auth cache invalidation from the safety plan.
- Implement the safety plan's `list_positions()` contract with the official paginated Data API client. Reuse the same normalized account filter and loader later used by settlement matching; wrong-account rows, malformed quantities, or incomplete pagination fail closed instead of appearing as an empty account.
- Implement the prerequisite safety trait's deadline-bearing `submit`, `cancel`, `replace`, health, list, reconcile, and settlement methods exactly. Client creation/version discovery, CLOB token/market lookup, Gamma corroboration, contract-config resolution, authentication/auth refresh, signing, post-order submission, and post-submit order/trade lookup all consume the caller's one absolute deadline. Before each await/retry, calculate the remaining budget; zero budget makes no call. No SDK future may outlive the synchronous call while the daemon mutation lock is held.

Step 1: Add failing tests.

Rust:

```rust
fn default_config_targets_v2_host()
fn v1_protocol_version_is_rejected_before_client_cache()
fn wallet_signature_type_accepts_poly1271()
fn poly1271_requires_explicit_funder()
fn v2_order_type_mapping_preserves_gtc_fak_fok()
fn geoblocked_readiness_fails_closed()
fn submit_rejects_before_signing_when_settlement_identity_is_missing()
fn acknowledged_v2_order_carries_complete_settlement_identity()
fn settlement_identity_rejects_gamma_condition_or_event_ambiguity()
fn settlement_identity_rejects_collateral_mismatch()
fn v2_identity_auth_sign_submit_and_followup_share_one_absolute_deadline()
fn expired_v2_deadline_starts_no_version_gamma_clob_auth_or_order_call()
```

Python contract:

```text
test_execution_uses_official_v2_crate_and_exact_version
test_default_clob_host_is_v2
test_legacy_sdk_is_not_a_workspace_member
test_retired_claimer_stays_absent
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-connectivity default_config_targets_v2_host --lib
rtk cargo test -p ploy-connectivity v1_protocol_version_is_rejected_before_client_cache --lib
rtk pytest tests/test_polymarket_v2_execution_contracts.py
```

Expected RED result: the default host is V1, imports reference the legacy package, and workspace metadata includes the vendored package.

Step 3: Migrate adapter code.

- Update SDK import paths and request/response field differences using compiler errors as the checklist.
- Keep `ExecutionRequest`, the safety plan's enriched `ExecutionOutcome` with immediate fills/residual truth, `CancellationOutcome`, `BulkCancellationOutcome`, `VenueOpenOrder`, `VenuePosition`, and `FillRecord` as Ploy-owned public types. Only the adapter internals change to V2 DTOs.
- Update `tools/sdk_auth_check` to compile against V2 only; tests compile it but do not execute authentication.
- Update the claim/redeem gate script to report two facts separately: official V2 execution adapter active, legacy market-data adapter temporarily retained.

Step 4: Verify dependency and runtime boundaries.

```bash
rtk cargo test -p ploy-connectivity --lib
rtk cargo check --locked -p new-ployd
rtk pytest tests/test_polymarket_v2_execution_contracts.py
cargo metadata --no-deps --format-version 1 | \
  python3 -c 'import json,sys; d=json.load(sys.stdin); assert all("polymarket-client-sdk" not in x for x in d["workspace_members"])'
cargo tree --locked -p ploy-connectivity --edges normal | rg 'polymarket_client_sdk_v2 v0\.6\.0'
! cargo tree --locked -p ploy-connectivity --edges normal | rg 'aws-sdk-kms|polymarket-client-sdk v'
scripts/check_v2_claim_redeem_gate.sh
rtk git diff --check
```

Expected result: connectivity has one official V2 dependency, no normal AWS KMS path, and no legacy workspace member.

The focused suite must include `v2_list_positions_satisfies_readiness_contract`, `v2_list_positions_rejects_wrong_account_rows`, and the safety plan's immediate-partial-fill/residual tests.

Step 5: Commit.

```bash
git add Cargo.toml Cargo.lock \
  crates/ploy-connectivity/Cargo.toml \
  crates/ploy-connectivity/src/lib.rs \
  tools/sdk_auth_check \
  tests/test_polymarket_v2_execution_contracts.py \
  scripts/check_v2_claim_redeem_gate.sh
git commit -m "feat(connectivity): migrate live execution to Polymarket V2"
```

---

### Task 2: Add the canonical resolution/redemption domain ledger

Files:

- Add `crates/ploy-trading/src/settlements.rs`.
- Modify `crates/ploy-trading/src/positions.rs`.
- Modify `crates/ploy-trading/src/runtime.rs`.
- Modify `crates/ploy-trading/src/lib.rs`.
- Modify `crates/ploy-platform-runtime/src/runtime_support.rs`.
- Modify `crates/ploy-platform-runtime/src/trade_submit.rs`.
- Modify `crates/ploy-daemon-host/src/runtime.rs`.
- Modify `crates/ploy-strategy-bundles/examples/run_backtest.rs`.

Domain types:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementProtocol {
    Binary,
    NegRisk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedemptionStatus {
    Resolved,
    Requested,
    PartiallyConfirmed,
    Confirmed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedemptionAttemptStatus {
    Requested,
    Confirmed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementIdentity {
    pub condition_id: String,
    pub event_id: String,
    pub token_id: String,
    pub protocol: SettlementProtocol,
    pub collateral_token: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedemptionAttempt {
    pub redemption_id: String,
    pub quantity: Decimal,
    pub redeem_request_id: Option<String>,
    pub idempotency_key: String,
    pub transaction_hash: Option<String>,
    pub relayer_receipt_id: Option<String>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub status: RedemptionAttemptStatus,
    pub retryable: bool,
    pub observed_at: DateTime<Utc>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettlementRecord {
    pub settlement_id: String,
    pub identity: SettlementIdentity,
    pub resolved_quantity: Decimal,
    pub confirmed_quantity: Decimal,
    pub payout: Decimal,
    pub resolution_source: String,
    pub resolved_at: DateTime<Utc>,
    pub status: RedemptionStatus,
    pub redemptions: Vec<RedemptionAttempt>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RedemptionReceipt {
    Confirmed {
        redemption_id: String,
        redeem_request_id: Option<String>,
        idempotency_key: String,
        quantity: Decimal,
        transaction_hash: Option<String>,
        relayer_receipt_id: Option<String>,
        confirmed_at: DateTime<Utc>,
    },
    Failed {
        redemption_id: String,
        redeem_request_id: Option<String>,
        idempotency_key: String,
        quantity: Decimal,
        reason: String,
        retryable: bool,
        observed_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettlementTransition {
    pub settlement_id: String,
    pub previous_status: RedemptionStatus,
    pub current_status: RedemptionStatus,
    pub changed: bool,
    pub confirmed_quantity: Decimal,
    pub realized_pnl: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SettlementError {
    #[error("invalid settlement identity: {0}")]
    InvalidIdentity(String),
    #[error("invalid settlement payout: {0}")]
    InvalidPayout(String),
    #[error("invalid settlement quantity: {0}")]
    InvalidQuantity(String),
    #[error("invalid redemption transition: {0}")]
    InvalidTransition(String),
    #[error("conflicting redemption receipt: {0}")]
    ReceiptConflict(String),
    #[error("canonical position conflict: {0}")]
    PositionConflict(String),
}
```

Ledger API:

```rust
impl SettlementLedger {
    pub fn restore(records: Vec<SettlementRecord>) -> Result<Self, SettlementError>;
    pub fn record_resolution(&mut self, record: SettlementRecord)
        -> Result<&SettlementRecord, SettlementError>;
    pub fn mark_requested(
        &mut self,
        settlement_id: &str,
        redemption_id: String,
        request_id: String,
        idempotency_key: String,
        quantity: Decimal,
    ) -> Result<&SettlementRecord, SettlementError>;
    pub fn apply_receipt(
        &mut self,
        settlement_id: &str,
        receipt: RedemptionReceipt,
    ) -> Result<SettlementTransition, SettlementError>;
    pub fn records(&self) -> impl Iterator<Item = &SettlementRecord>;
}

impl TradingRuntime {
    pub fn register_settlement_identity(
        &mut self,
        identity: SettlementIdentity,
    ) -> Result<(), TradingRuntimeError>;
    pub fn record_resolution(&mut self, record: SettlementRecord)
        -> Result<&SettlementRecord, TradingRuntimeError>;
    pub fn mark_redemption_requested(
        &mut self,
        settlement_id: &str,
        redemption_id: String,
        request_id: String,
        idempotency_key: String,
        quantity: Decimal,
    )
        -> Result<&SettlementRecord, TradingRuntimeError>;
    pub fn apply_redemption_receipt(
        &mut self,
        settlement_id: &str,
        receipt: RedemptionReceipt,
    )
        -> Result<SettlementTransition, TradingRuntimeError>;
}
```

Rules:

- Valid payout is exactly `0`, `0.5`, or `1`.
- `resolved_quantity` is positive and no greater than the current positive canonical token position at first official resolution observation; a later fill for an already resolved token is a conflict.
- Resolution and requested status do not change the position, order, or fill ledger.
- Every request/receipt has a positive `quantity` no greater than `resolved_quantity - confirmed_quantity`; confirmed attempts accumulate without exceeding the resolution aggregate.
- Failed, reverted, missing, or timed-out attempts preserve position. With zero confirmed quantity the aggregate may be `Failed`; after a partial confirmation it remains `PartiallyConfirmed` while retaining retryable failure detail.
- State transitions are `Resolved -> Requested|PartiallyConfirmed|Confirmed|Failed`; retryable attempts may move `Requested|Failed -> Requested|PartiallyConfirmed|Confirmed|Failed`. `PartiallyConfirmed` may accept further attempts. `Confirmed` is terminal except an identical receipt replay.
- Confirmed receipt requires a stable non-empty `redemption_id`, idempotency key, positive quantity, a non-empty transaction hash or relayer receipt ID, and confirmation time.
- On each first-seen confirmed redemption ID, `PositionLedger::apply_confirmed_redemption` reduces only that receipt's quantity and realizes `(payout - avg_entry_price) * quantity`.
- Full redemption sets net quantity and average entry to zero, releasing gross exposure naturally.
- Replaying the same settlement ID/redemption ID/idempotency/receipt is a no-op. Reusing any of those keys with different quantity or evidence is a conflict.
- No code creates a synthetic SELL `FillRecord`.

Step 1: Add failing tests.

```rust
fn resolution_does_not_close_position()
fn requested_redemption_does_not_close_position()
fn confirmed_winning_redemption_closes_position_and_realizes_pnl()
fn confirmed_zero_payout_closes_position_at_zero()
fn confirmed_half_payout_realizes_half_value()
fn failed_or_reverted_receipt_preserves_position()
fn failed_then_requested_then_confirmed_retry_releases_position_once()
fn partial_confirmed_redemption_preserves_remaining_average_entry()
fn multiple_partial_confirmations_accumulate_to_full_once()
fn duplicate_confirmed_receipt_is_idempotent()
fn conflicting_receipt_after_confirmation_is_rejected()
fn redemption_cannot_exceed_positive_open_quantity()
fn confirmed_redemption_releases_exposure_without_sell_fill()
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-trading resolution_does_not_close_position --lib
rtk cargo test -p ploy-trading confirmed_half_payout_realizes_half_value --lib
rtk cargo test -p ploy-trading confirmed_redemption_releases_exposure_without_sell_fill --lib
```

Expected RED result: no settlement ledger exists and positions can change only through fills.

Step 3: Implement the minimal aggregate.

- Use `BTreeMap<String, SettlementRecord>` for deterministic snapshots.
- Implement `records()` as the map's value iterator; snapshot code collects it into a `Vec`. Do not promise a contiguous slice from map-backed storage.
- Keep validation/transition logic in `settlements.rs`; keep quantity/PnL mutation in `positions.rs`.
- Add `#[serde(default)] pub settlements: Vec<SettlementRecord>` to `TradingRuntimeSnapshot` for old snapshot compatibility.
- Add `#[serde(default)] pub settlement_identities: Vec<SettlementIdentity>` to `TradingRuntimeSnapshot`; an old paper snapshot may omit it, but a live position without identity is not settlement-ready.
- Change `TradingRuntime::restore(snapshot)` to `Result<TradingRuntime, TradingRuntimeError>`. Replay fills first and confirmed settlement records second, reject invalid transitions/identity/position mismatches, and propagate the result through every caller listed in this task rather than panicking or silently dropping records.
- Map the V2 acknowledgement's `VenueSettlementIdentity` into the domain identity and register/persist it before applying immediate fills.

Step 4: Verify.

```bash
rtk cargo test -p ploy-trading settlement --lib
rtk cargo test -p ploy-trading restore --lib
rtk cargo test -p ploy-trading --lib
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-trading/src/settlements.rs \
  crates/ploy-trading/src/positions.rs \
  crates/ploy-trading/src/runtime.rs \
  crates/ploy-trading/src/lib.rs \
  crates/ploy-platform-runtime/src/runtime_support.rs \
  crates/ploy-platform-runtime/src/trade_submit.rs \
  crates/ploy-daemon-host/src/runtime.rs \
  crates/ploy-strategy-bundles/examples/run_backtest.rs
git commit -m "feat(trading): record confirmed redemption lifecycle"
```

---

### Task 3: Persist settlement state through operator contracts and the canonical PostgreSQL store

Files:

- Modify `crates/ploy-operator-contracts/src/trading.rs`.
- Modify `crates/ploy-operator-contracts/src/schemas.rs`.
- Regenerate `contracts/schemas/trading-state-snapshot.schema.json`.
- Modify `crates/ploy-platform-runtime/src/runtime_support.rs`.
- Modify `crates/ploy-platform-runtime/src/state_io.rs`.
- Modify `apps/ployctl/src/trading.rs`.
- Modify `apps/ploytui/src/lib.rs`.
- Modify `crates/ploy-control-client/src/lib.rs`.
- Modify `crates/ploy-daemon-host/src/runtime.rs`.
- Modify `crates/ploy-operator-contracts/src/events.rs`.
- Modify `crates/ploy-strategy-runtime/src/live.rs`.
- Add `migrations/049_canonical_runtime_state.sql`.
- Add `crates/ploy-daemon-host/src/canonical_store.rs`.
- Modify `crates/ploy-daemon-host/src/lib.rs`.
- Modify `crates/ploy-daemon-host/src/config.rs`.
- Modify `crates/ploy-daemon-host/src/audit_io.rs`, added by the prerequisite live-safety slice.
- Modify `crates/ploy-daemon-host/src/http.rs`.
- Modify `crates/ploy-daemon-host/Cargo.toml`.
- Add `crates/ploy-daemon-host/tests/canonical_store_postgres.rs`.
- Modify `apps/new-ployd/tests/sigterm.rs` from the prerequisite live-safety slice.
- Add `tests/test_canonical_runtime_store_contracts.py`.
- Modify `.github/workflows/test.yml` to execute the ignored PostgreSQL contract test against its existing Postgres service.
- Regenerate `ploy-frontend/src/types/operator-contracts.ts` and `ploy-sidecar/src/contracts/operator-contracts.ts` with `scripts/export_operator_contract_types.mjs`.

Wire type:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettlementSnapshot {
    pub settlement_id: String,
    pub condition_id: String,
    pub event_id: String,
    pub token_id: String,
    pub protocol: String,
    pub collateral_token: String,
    pub resolved_quantity: Decimal,
    pub confirmed_quantity: Decimal,
    pub payout: Decimal,
    pub resolution_source: String,
    pub resolved_at: DateTime<Utc>,
    pub status: String,
    pub redemptions: Vec<RedemptionAttemptSnapshot>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RedemptionAttemptSnapshot {
    pub redemption_id: String,
    pub quantity: Decimal,
    pub redeem_request_id: Option<String>,
    pub idempotency_key: String,
    pub transaction_hash: Option<String>,
    pub relayer_receipt_id: Option<String>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub status: String,
    pub retryable: bool,
    pub observed_at: DateTime<Utc>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SettlementIdentitySnapshot {
    pub condition_id: String,
    pub event_id: String,
    pub token_id: String,
    pub protocol: String,
    pub collateral_token: String,
}
```

Add `#[serde(default)] pub settlements: Vec<SettlementSnapshot>` and `#[serde(default)] pub settlement_identities: Vec<SettlementIdentitySnapshot>` to the existing `TradingStateSnapshot`. Update every Rust struct literal in the same commit; serde defaults protect old JSON, not Rust source construction.

Canonical store boundary:

```rust
pub enum CanonicalStoreMode {
    File,
    Postgres,
}

pub struct CanonicalRuntimeEnvelope {
    pub deployment: DeploymentRecord,
    pub trading: TradingStateSnapshot,
    pub state_version: i64,
}

pub struct CanonicalCommitRequest {
    pub expected_versions: BTreeMap<String, i64>,
    pub states: Vec<CanonicalRuntimeEnvelope>,
    pub audit_entries: Vec<CanonicalAuditEntry>,
}

pub struct CanonicalAuditEntry {
    pub audit_id: String,
    pub scope_id: String,
    pub event_type: String,
    pub idempotency_key: String,
    pub committed_versions: BTreeMap<String, i64>,
    pub entry: AuditLogEntry,
}

pub trait CanonicalRuntimeStore: Send + Sync + Debug {
    fn load_all(&self) -> io::Result<Vec<CanonicalRuntimeEnvelope>>;
    fn commit(&self, request: CanonicalCommitRequest) -> io::Result<()>;
    fn audit_entries(&self, limit: usize) -> io::Result<Vec<AuditLogEntry>>;
}

pub struct PloyDaemon {
    // existing fields
    canonical_store: Arc<dyn CanonicalRuntimeStore>,
    canonical_state_versions: BTreeMap<String, i64>,
}

impl PloyDaemon {
    pub fn boot_with_canonical_store(
        config: &PlatformConfig,
        gateway: Box<dyn LiveExecutionGateway>,
        canonical_store: Arc<dyn CanonicalRuntimeStore>,
    ) -> io::Result<Self>;
}
```

Migration `049` adds:

```text
ploy_runtime_state(
  deployment_id primary key,
  state_version bigint not null check (state_version > 0),
  deployment_json jsonb not null,
  trading_json jsonb not null,
  snapshot_sha256 text not null,
  updated_at timestamptz not null
)

ploy_runtime_audit(
  audit_id text primary key,
  scope_id text not null,
  committed_versions jsonb not null,
  event_type text not null,
  idempotency_key text not null,
  payload jsonb not null,
  created_at timestamptz not null,
  unique(scope_id, idempotency_key)
)
```

Rules:

- `build_trading_state_snapshot()` writes every settlement transition field.
- `restore_trading_runtime()` rebuilds positions from fills plus every confirmed redemption attempt, then compares the rebuilt positions to persisted positions.
- Old snapshots lacking `settlements` deserialize as empty.
- Invalid protocol/status/payout/receipt combinations fail restore.
- `ployctl trading status` prints `settlements=<count>` and `confirmed_redemptions=<count>`.
- `PLOY_CANONICAL_STORE=file` is the default only for current local paper compatibility. Applying or restoring any non-archived live deployment in file mode fails closed before a worker or venue client starts.
- `PLOY_CANONICAL_STORE=postgres` requires `PLOY_DATABASE__URL` with TLS mode `require`, `verify-ca`, or `verify-full`; missing/invalid URL or database unavailability fails daemon boot. The trade deploy verifier later requires this mode.
- PostgreSQL `commit()` begins one transaction, locks all affected deployment IDs in sorted order, compares every expected version, writes every state row, and appends every audit row before commit. Version conflict, duplicate idempotency with different payload, serialization error, or audit insert failure rolls the whole transaction back.
- Every audit entry explicitly carries the sorted map of deployment IDs to the new versions committed by that event. Deployment-scoped events require exactly their one matching ID/version. Account/system events such as emergency stop carry every affected live deployment/version; an empty map is permitted only for a proven no-state-change system receipt. Unknown IDs, old/future versions, missing affected IDs, scope mismatch, or a multi-deployment map that differs from the same transaction's state rows rejects the whole commit.
- A missing row has expected version `0` and is inserted at version `1`; an existing row at `n` must be committed as `n + 1`. The store rejects skipped/reused versions, duplicate deployment IDs in one request, or state/audit scope mismatch.
- Compute `snapshot_sha256` from canonical serialized deployment+trading JSON before commit and verify it on every load; hash mismatch blocks boot instead of trusting either database JSON or file cache.
- Every pending-before-submit, acknowledgement/fill, cancel/replace, reconciliation, emergency pause, and settlement transition uses that one store boundary. A venue mutation is never reported successful before the matching canonical commit.
- In PostgreSQL mode, write JSON/JSONL caches only after the database commit. Cache failure records a critical alert and returns no success, but restart loads PostgreSQL and rewrites the cache; cache contents never win a conflict.
- `/api/audit/logs` reads the canonical store in PostgreSQL mode. The file reader remains only for file-mode compatibility.
- The Postgres adapter owns a bounded one-connection Tokio runtime/pool behind the synchronous trait. Connect/acquire/query deadlines are finite; no caller blocks indefinitely while holding the daemon mutation lock.
- Production boot is the only constructor that selects file/PostgreSQL from `PlatformConfig`. `boot_with_canonical_store` preserves the existing boxed gateway ownership and adds only an `Arc` store dependency-injection seam for deterministic tests/process fixtures; it does not consult environment mode and is never selected by production `main`.
- Add a deterministic in-memory fake store implementing optimistic versions, atomic commit, idempotency conflict, audit reads, and injectable failures. Every unit/integration fixture containing a non-archived live deployment injects this fake, including `apps/new-ployd/tests/sigterm.rs`; file-store tests alone exercise the intentional live rejection. Re-scan `rg -n 'boot_with_live_execution|runtime_mode.*live|RuntimeMode::Live' --glob '*.rs'` and update all matching live fixtures in the same commit so workspace CI never needs local PostgreSQL.

Step 1: Add failing tests.

```rust
fn old_snapshot_without_settlements_deserializes()
fn trading_state_snapshot_uses_stable_settlement_wire_keys()
fn restore_reconstructs_positions_from_fills_and_confirmed_redemptions()
fn restore_rejects_position_mismatch_after_redemption()
fn state_io_restart_preserves_redemption_idempotency()
fn ployctl_renders_settlement_and_confirmation_counts()
fn file_store_rejects_non_archived_live_state()
fn canonical_commit_is_atomic_in_fake_store()
fn postgres_version_conflict_rolls_back_state_and_audit()
fn postgres_duplicate_idempotency_with_different_payload_fails()
fn multi_deployment_emergency_state_and_version_map_commit_atomically()
fn audit_committed_version_scope_mismatch_rolls_back_every_state()
fn postgres_boot_state_overrides_stale_json_cache()
fn postgres_unavailable_blocks_live_boot()
fn injected_fake_store_supports_live_sigterm_fixture_without_database()
fn every_live_fixture_uses_explicit_canonical_store()
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-operator-contracts old_snapshot_without_settlements_deserializes
rtk cargo test -p ploy-platform-runtime restore_reconstructs_positions_from_fills_and_confirmed_redemptions --lib
rtk cargo test -p ployctl ployctl_renders_settlement_and_confirmation_counts
```

Expected RED result: the wire snapshot has no settlement field, restore replays fills only, and daemon persistence is file-only.

Step 3: Implement conversion and validation.

- Add explicit protocol/status/attempt wire conversion functions next to existing order/side conversion functions.
- Never trust persisted positions directly; rebuild and compare.
- Use dynamic `sqlx::query` calls rather than compile-time macros so local compilation needs no database. Mark the real PostgreSQL integration test ignored by default; the existing `rust-control-plane` CI job runs it explicitly after `sqlx migrate run` against its service container.

Step 4: Regenerate and verify.

```bash
cargo run -p ploy-operator-contracts --example export_schemas
node scripts/export_operator_contract_types.mjs
rtk cargo test -p ploy-operator-contracts
rtk cargo test -p ploy-platform-runtime runtime_support --lib
rtk cargo test -p ploy-platform-runtime state_io --lib
rtk cargo test -p ploy-daemon-host canonical_store --lib
rtk pytest tests/test_canonical_runtime_store_contracts.py
rtk cargo test -p ployctl trading
npm run contracts:check --prefix ploy-frontend
npm run contracts:check --prefix ploy-sidecar
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-operator-contracts/src/trading.rs \
  crates/ploy-operator-contracts/src/schemas.rs \
  contracts/schemas/trading-state-snapshot.schema.json \
  crates/ploy-platform-runtime/src/runtime_support.rs \
  crates/ploy-platform-runtime/src/state_io.rs \
  apps/ployctl/src/trading.rs \
  apps/ploytui/src/lib.rs \
  crates/ploy-control-client/src/lib.rs \
  crates/ploy-daemon-host/src/runtime.rs \
  crates/ploy-daemon-host/src/canonical_store.rs \
  crates/ploy-daemon-host/src/lib.rs \
  crates/ploy-daemon-host/src/config.rs \
  crates/ploy-daemon-host/src/audit_io.rs \
  crates/ploy-daemon-host/src/http.rs \
  crates/ploy-daemon-host/Cargo.toml \
  crates/ploy-daemon-host/tests/canonical_store_postgres.rs \
  apps/new-ployd/tests/sigterm.rs \
  migrations/049_canonical_runtime_state.sql \
  tests/test_canonical_runtime_store_contracts.py \
  .github/workflows/test.yml \
  crates/ploy-operator-contracts/src/events.rs \
  crates/ploy-strategy-runtime/src/live.rs \
  ploy-frontend/src/types/operator-contracts.ts \
  ploy-sidecar/src/contracts/operator-contracts.ts
git commit -m "feat(persistence): make PostgreSQL trading state canonical"
```

---

### Task 4: Reconcile official resolution separately from confirmed redeem evidence

Files:

- Modify `crates/ploy-connectivity/src/lib.rs`.
- Modify `crates/ploy-daemon-host/src/runtime.rs`.
- Modify `crates/ploy-platform-runtime/src/trade_control.rs`.
- Modify `crates/ploy-platform-runtime/src/trade_submit.rs`.

Ploy gateway types:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct TrackedPosition {
    pub account_id: String,
    pub deployment_id: String,
    pub identity: VenueSettlementIdentity,
    pub quantity: Decimal,
    pub first_fill_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VenueResolutionEvidence {
    pub resolution_id: String,
    pub identity: VenueSettlementIdentity,
    pub payout: Decimal,
    pub resolution_source: String,
    pub resolved_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VenueRedemptionEvidence {
    pub redemption_id: String,
    pub account_id: String,
    pub token_id: String,
    pub quantity: Decimal,
    pub usdc_size: Decimal,
    pub redeem_request_id: Option<String>,
    pub transaction_hash: Option<String>,
    pub relayer_receipt_id: Option<String>,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub failure: Option<VenueRedemptionFailure>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VenueSettlementEvidence {
    pub resolution: VenueResolutionEvidence,
    pub redemptions: Vec<VenueRedemptionEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueRedemptionFailure {
    pub reason: String,
    pub retryable: bool,
    pub observed_at: DateTime<Utc>,
}

fn reconcile_settlements(
    &self,
    tracked_positions: &[TrackedPosition],
    deadline: Instant,
) -> Result<Vec<VenueSettlementEvidence>, ExecutionError>;
```

Official resolution rules:

- For each persisted tracked identity, load the official CLOB market by condition ID and the exactly matching Gamma market. Require CLOB `closed=true`, the same condition/token membership/protocol, Gamma `closed=true`, a parseable official `closed_time`, and matching final outcome arrays. `redeemable=true`, a closed Data API position, or a redeem activity is not resolution proof.
- For an ordinary binary market, require exactly one CLOB token with `winner=true`; its payout is `1` and every other token is `0`. Gamma outcome/token ordering and final `outcome_prices` must corroborate those exact values.
- For a 50/50 resolution, require CLOB `is_50_50_outcome=true`, no contradictory winner flags, both CLOB final token prices exactly `0.5`, and Gamma final outcome prices exactly `0.5`. Any other payout or source disagreement is invalid response evidence.
- Construct `resolution_id` deterministically from normalized condition ID, token ID, payout, and official closed timestamp. `resolution_source` names the corroborated official CLOB+Gamma boundary; `resolved_at` is the parsed official close/resolution timestamp.
- Missing, ambiguous, non-final, or conflicting official resolution returns a concrete gateway error and degrades reconciliation; it is never replaced by Data API cashflow arithmetic.

Redemption confirmation rules:

- Load the normalized account's current positions, closed positions, and `REDEEM` activities through the official Data API client.
- A valid official resolution with no redeem evidence produces `VenueSettlementEvidence { resolution, redemptions: vec![] }`.
- A closed position without matching redeem activity is unconfirmed.
- A redeem activity without matching account, canonical token identity, and positive tracked quantity is unconfirmed. A remaining open position is valid for partial redemption; the activity quantity must be positive and no greater than the tracked canonical quantity.
- A confirmed event requires a non-zero Data API `B256` transaction hash or a non-empty authenticated relayer receipt ID, plus a timestamp not earlier than the tracked position's first fill. Zero hashes and empty strings normalize to `None`; both identifiers missing is unconfirmed.
- Confirmation fields and `failure` are mutually exclusive. Authenticated relayer failure/revert/timeout evidence produces a failed domain transition without releasing quantity; ordinary absence of activity remains resolved/unconfirmed rather than fabricating failure.
- Data API `REDEEM` activity normally supplies the transaction hash. Relayer-only confirmation enters through the same pure matching boundary as typed gateway evidence and must carry a stable authenticated receipt ID; local tests use deterministic fake evidence and make no relayer call.
- Derive `redeem_request_id` and `redemption_id` from a typed evidence key: `tx:<transaction_hash>` when a transaction hash exists, otherwise `relayer:<relayer_receipt_id>`, followed by `:<token_id>`. This keeps transaction and relayer namespaces collision-free.
- Redemption does not determine payout. Validate the cashflow only by requiring exact `size * resolution.payout == usdc_size`; the SDK activity `price` field is trade-only and ignored. Reject zero/negative size, negative USDC size, multiplication overflow, or any mismatch with the separately proven official payout.
- Copy condition/event/protocol/collateral identity from the persisted `TrackedPosition.identity`. Data API DTOs corroborate account/token/activity only and never invent missing canonical identity.
- Duplicate Data API rows collapse to one venue event.
- Paginate all three surfaces; a first page is not confirmation truth.
- Use a configurable mockable Data API host. Unit tests bind a local listener and never access the public API.

Step 1: Add failing tests.

```rust
fn redeemable_open_position_is_not_confirmation()
fn resolution_without_redeem_activity_stays_unconfirmed()
fn official_binary_resolution_proves_zero_and_one_payouts()
fn official_fifty_fifty_resolution_proves_half_payout()
fn resolution_rejects_clob_gamma_payout_or_identity_disagreement()
fn closed_position_without_redeem_activity_stays_unconfirmed()
fn redeem_activity_without_matching_tracked_position_stays_unconfirmed()
fn partial_redeem_activity_matches_while_remaining_position_is_open()
fn one_resolution_groups_multiple_partial_redeem_activities()
fn conflicting_duplicate_redemption_id_fails_closed()
fn matched_redeem_activity_emits_confirmed_binary_evidence()
fn matched_half_payout_activity_corroborates_official_half_resolution()
fn relayer_receipt_without_transaction_hash_is_confirmed()
fn missing_transaction_and_relayer_receipt_is_unconfirmed()
fn relayer_failure_is_retryable_and_never_confirms_quantity()
fn unrelated_wallet_condition_or_asset_is_ignored()
fn redeem_activity_before_first_fill_is_ignored()
fn duplicate_activity_produces_one_venue_event()
fn settlement_data_api_paginates_every_surface()
fn redeem_cashflow_must_match_official_payout_and_ignores_trade_price()
fn tracked_position_identity_is_copied_without_dto_guessing()
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-connectivity redeemable_open_position_is_not_confirmation --lib
rtk cargo test -p ploy-connectivity matched_redeem_activity_emits_confirmed_binary_evidence --lib
rtk cargo test -p ploy-connectivity official_fifty_fifty_resolution_proves_half_payout --lib
```

Expected RED result: the gateway has no settlement reconciliation surface.

Step 3: Implement pure matching first.

- Put official resolution matching and redemption matching in two pure private functions over SDK response slices, then join them only by the persisted identity.
- Add `StaticExecutionGateway::with_settlement_result` and update all exact fake implementation files listed above; do not add a default empty trait implementation.
- Keep raw SDK DTOs inside connectivity.
- Do not enable `ctf`, construct a provider, or send an account operation.

Step 4: Verify.

```bash
rtk cargo test -p ploy-connectivity confirmed_v2_settlements --lib
rtk cargo test -p ploy-connectivity settlement_data_api_paginates_every_surface --lib
rtk cargo test -p ploy-connectivity --lib
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-connectivity/src/lib.rs \
  crates/ploy-daemon-host/src/runtime.rs \
  crates/ploy-platform-runtime/src/trade_control.rs \
  crates/ploy-platform-runtime/src/trade_submit.rs
git commit -m "feat(connectivity): reconcile confirmed redeem evidence"
```

---

### Task 5: Apply confirmed redemption exactly once across deployments

Files:

- Modify `crates/ploy-platform-runtime/src/reconcile.rs`.
- Modify `crates/ploy-platform-runtime/src/runtime_support.rs`.
- Modify `crates/ploy-platform-runtime/src/lib.rs`.
- Modify `crates/ploy-daemon-host/src/runtime.rs`.

Reconcile result:

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileCounts {
    pub fills: usize,
    pub resolutions: usize,
    pub confirmed_redemptions: usize,
    pub failed_redemptions: usize,
    pub unmanaged_positions: usize,
}

pub enum ReconcileStatus {
    Applied(ReconcileCounts),
    Degraded(ReconcileCounts),
    Noop,
    BackoffActive,
}

pub fn reconcile_live_state(
    gateway: &dyn LiveExecutionGateway,
    deployments: &[DeploymentRecord],
    trading: &mut BTreeMap<String, TradingRuntime>,
) -> io::Result<ReconcileStatus>;

pub fn reconcile_live_state_until(
    gateway: &dyn LiveExecutionGateway,
    deployments: &[DeploymentRecord],
    trading: &mut BTreeMap<String, TradingRuntime>,
    deadline: Instant,
) -> io::Result<ReconcileStatus>;
```

The ordinary wrapper creates the same 5,000 ms absolute monotonic deadline as the live-safety slice and delegates to `reconcile_live_state_until`. The `_until` form applies the one shared budget across health, fills, official resolution, redemption, open orders, and positions. Emergency stop is updated to call `_until` with its existing shared deadline; it never resets the budget between phases.

Allocation rules:

- Scan only positive positions in non-archived live deployments.
- All scanned live deployments must have the same normalized account ID; otherwise return a configuration error.
- When the same account/token is held by multiple deployments, sort by `deployment_id` and allocate venue quantity without duplication.
- Per-deployment settlement ID is `<resolution_id>:<deployment_id>` and represents the full quantity held by that deployment when official resolution is first observed.
- Cap venue quantity to canonical quantity. Extra venue quantity is external/unmanaged and creates a critical degraded result, not extra PnL.
- Record resolution for every valid official resolution row even when `redemptions` is empty.
- Group all matching historical redeem activities by canonical resolution before returning gateway evidence, sort/deduplicate them by `redemption_id`, and return every distinct partial receipt in `redemptions`. Repeating resolution metadata across polling pages is one resolution group, never multiple domain resolutions; conflicting duplicates degrade reconciliation.
- Allocate each redemption quantity across matching deployment settlement aggregates in sorted `deployment_id` order. Derive the domain attempt idempotency key exactly as `<settlement_id>:<redemption_id>` after allocation; never use a transaction hash alone and never let an adapter invent a different key. Apply position mutation only when transaction/relayer confirmation fields satisfy the domain receipt contract; multiple partial receipts accumulate by unique redemption ID without exceeding the aggregate's remaining quantity.
- A redeem cashflow that disagrees with the official payout is a critical degraded result and applies neither resolution mutation beyond already-proven state nor redemption quantity.
- Apply authenticated failure evidence as `RedemptionReceipt::Failed`, preserve quantity/exposure, mark the deployment/account venue source degraded with a retryable error, and keep it eligible for a later requested/confirmed retry.
- A confirmed receipt after a failed retry applies exactly once and clears the redemption-specific degraded source only after open-order/position readiness also passes.
- Persist trading state before reporting a successful applied reconcile.
- Restart and repeated poll are idempotent.
- One poll returning two or more partial redemption receipts for the same resolution applies each exactly once, persists all attempts atomically, and cannot replay them across multiple deployments on the next poll.

Step 1: Add failing tests.

```rust
fn reconcile_resolution_without_receipt_retains_position_and_exposure()
fn reconcile_applies_confirmed_redemption_and_releases_exposure()
fn reconcile_partial_redemptions_accumulate_without_double_release()
fn one_poll_with_two_partial_receipts_applies_each_once_across_deployments()
fn reconcile_rejects_redeem_cashflow_that_conflicts_with_official_resolution()
fn confirmed_redemption_does_not_create_fill_or_change_order()
fn duplicate_confirmation_is_noop_after_restart()
fn confirmation_quantity_is_allocated_once_across_deployments()
fn venue_quantity_above_canonical_position_is_capped_and_degraded()
fn failed_redemption_preserves_exposure_and_marks_retryable_degraded()
fn failed_then_confirmed_reconcile_releases_position_exactly_once()
fn unknown_external_token_position_blocks_readiness()
fn multiple_live_account_ids_fail_closed()
fn daemon_restart_restores_redeemed_position_and_pnl()
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-platform-runtime reconcile_resolution_without_receipt_retains_position_and_exposure --lib
rtk cargo test -p ploy-platform-runtime reconcile_applies_confirmed_redemption_and_releases_exposure --lib
rtk cargo test -p ploy-daemon-host daemon_restart_restores_redeemed_position_and_pnl --lib
```

Expected RED result: reconcile only applies fills and cannot release settled positions.

Step 3: Implement and preserve health semantics.

- Replace the live-safety `reconcile_live_fills`/`reconcile_live_fills_until` pair with `reconcile_live_state`/`reconcile_live_state_until`, update every ordinary and emergency caller in the same commit, and remove the old names before commit.
- Count confirmed redemption as venue activity for persistence, not as a fill.
- Settlement reconciliation failure follows the same backoff/degraded path as fill reconciliation.
- A business-level failed/reverted receipt is counted and persisted before returning a degraded result; it is not collapsed into a transport error or `Noop`.

Step 4: Verify.

```bash
rtk cargo test -p ploy-platform-runtime reconcile --lib
rtk cargo test -p ploy-daemon-host settlement --lib
rtk cargo test -p ploy-daemon-host reconcile --lib
rtk cargo test -p ploy-daemon-host --lib
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-platform-runtime/src/reconcile.rs \
  crates/ploy-platform-runtime/src/runtime_support.rs \
  crates/ploy-platform-runtime/src/lib.rs \
  crates/ploy-daemon-host/src/runtime.rs
git commit -m "feat(runtime): reconcile confirmed redemption idempotently"
```

---

### Task 6: Add the canonical live-promotion operation and run the complete local matrix

Files:

- Modify `crates/ploy-connectivity/Cargo.toml` and `src/lib.rs` with the explicit `ploy-operator-contracts` dependency and typed account readiness under the shared deadline.
- Modify every existing `LiveExecutionGateway` implementation/fake, including `crates/ploy-platform-runtime/src/trade_submit.rs`, `crates/ploy-platform-runtime/src/trade_control.rs`, `crates/ploy-daemon-host/src/runtime.rs`, and `apps/new-ployd/tests/sigterm.rs` at the approved base.
- Modify `crates/ploy-operator-contracts/src/system.rs`, `src/lib.rs`, and `src/schemas.rs`.
- Add generated `contracts/schemas/recorded-runtime-parity-v2.schema.json`, `restart-recovery-receipt.schema.json`, `live-promotion-request.schema.json`, and `live-promotion-response.schema.json`; this task owns the strict parity wire type that the later Agent workflow must produce.
- Regenerate `ploy-frontend/src/types/operator-contracts.ts` and `ploy-sidecar/src/contracts/operator-contracts.ts`.
- Add `migrations/050_live_promotion_approvals.sql`.
- Modify `crates/ploy-daemon-host/src/canonical_store.rs`, `src/config.rs`, `src/runtime.rs`, and `src/http.rs`.
- Modify `crates/ploy-control-client/src/lib.rs` and `apps/ployctl/src/system.rs`/command parser.
- Modify `scripts/validate_live_promotion_gate.py`, `scripts/drills/pm5d_threelayer_live_gate.sh`, and `tests/test_live_promotion_gate.py` from Live Task 7.
- Add `tests/test_live_promotion_contracts.py`.
- Modify `docs/operations/v2-claim-redeem-gate.md`.
- Modify `docs/runbooks/live-deployment-checklist.md`.
- Modify `tasks/todo.md` with exact test results.

Canonical API:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordedRuntimeParityV2 {
    pub schema_version: String, // exact "recorded-runtime-parity.v2"
    pub parity_id: String,
    pub source_workflow: String,
    pub source_run_id: String,
    pub source_head_sha: String,
    pub candidate_id: String,
    pub candidate_replay_sha256: String,
    pub dry_run_evidence_sha256: String,
    pub symbols: Vec<String>,
    pub strategy_profile: String,
    pub runtime_score: String,
    pub market_window_secs: u32,
    pub prediction_horizon_secs: u32,
    pub entry_offset_secs: u32,
    pub target_label: String,
    pub accounting_lane: String,
    pub settlement_source: String,
    pub dry_run_config_sha256: String,
    pub expected_live_config_sha256: String,
    pub live_config_materialized: bool,
    pub model_sha256: String,
    pub runner_git_sha: String,
    pub recording_sha256: String,
    pub executable_cost: Decimal,
    pub average_entry: Decimal,
    pub max_drawdown: Decimal,
    pub bankroll: Decimal,
    pub fees_bps: Decimal,
    pub slippage_bps: Decimal,
    pub latency_ms: u64,
    pub max_account_exposure_usd: Decimal,
    pub strict_parity_ready: bool,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HumanLiveApproval {
    pub schema_version: String, // exact "human-live-approval.v1"
    pub approval_id: String,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub acknowledgement: String,
    pub account_id: String,
    pub max_account_exposure_usd: Decimal,
    pub parity_content_sha256: String,
    pub candidate_id: String,
    pub candidate_replay_sha256: String,
    pub expected_live_config_sha256: String,
    pub model_sha256: String,
    pub runner_git_sha: String,
    pub rds_recovery_proof_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RdsRecoveryProofReference {
    pub workflow: String,
    pub run_id: String,
    pub artifact_name: String,
    pub artifact_sha256: String,
    pub source_head_sha: String,
    pub verified_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProtectedWorkflowProvenanceUnsigned {
    pub schema_version: String, // exact "protected-live-provenance.v1"
    pub workflow: String,
    pub run_id: String,
    pub run_url: String,
    pub head_sha: String,
    pub parity_artifact_name: String,
    pub parity_artifact_sha256: String,
    pub parity_content_sha256: String,
    pub approval_sha256: String,
    pub expected_live_config_sha256: String,
    pub rds_recovery: RdsRecoveryProofReference,
    pub emergency_stop_audit_id: String,
    pub restart_recovery_audit_id: String,
    pub issued_at: DateTime<Utc>,
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProtectedWorkflowProvenance {
    pub unsigned: ProtectedWorkflowProvenanceUnsigned,
    pub hmac_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LivePromotionRequest {
    pub approval: HumanLiveApproval,
    pub parity: RecordedRuntimeParityV2,
    pub provenance: ProtectedWorkflowProvenance,
    pub emergency_stop_audit_id: String,
    pub restart_recovery_audit_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VenueAccountReadiness {
    pub account_id: String,
    pub collateral_token: String,
    pub geoblocked: bool,
    pub collateral_balance: Decimal,
    pub collateral_allowance: Decimal,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestartRecoveryReceipt {
    pub audit_id: String,
    pub emergency_stop_audit_id: String,
    pub prior_daemon_boot_id: String,
    pub current_daemon_boot_id: String,
    pub unresolved_orders: usize,
    pub unexplained_positions: usize,
    pub venue_health_fresh: bool,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LivePromotionResponse {
    pub success: bool,
    pub approval_id: String,
    pub deployment_id: String,
    pub desired_state: String,
    pub observed_state: String,
    pub approval_consumed: bool,
    pub state_versions: BTreeMap<String, i64>,
    pub audit_id: String,
    pub errors: Vec<String>,
}

impl PloyDaemon {
    pub fn approve_and_resume_live_canary_until(
        &mut self,
        request: LivePromotionRequest,
        deadline: Instant,
    ) -> io::Result<LivePromotionResponse>;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalLivePromotionRecord {
    pub approval: HumanLiveApproval,
    pub parity_content_sha256: String,
    pub provenance: ProtectedWorkflowProvenance,
    pub request_sha256: String,
    pub status: String,
    pub result: Option<LivePromotionResponse>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct CanonicalLivePromotionMutation {
    pub expected_absent: bool,
    pub record: CanonicalLivePromotionRecord,
}

// Add inside the existing LiveExecutionGateway trait from Live Task 3.
fn account_readiness(
    &self,
    account_id: &str,
    collateral_token: &str,
    deadline: Instant,
) -> Result<VenueAccountReadiness, ExecutionError>;
```

Rules:

- Immediately before implementation run `rg -l 'impl LiveExecutionGateway' --glob '*.rs'` and add every result to this task. `account_readiness` is required with no default/no-op body; each fake declares an explicit balance/allowance/geoblock result and records deadline calls. A newly added implementation is not exempt from the scan.
- Extend Task 3's `CanonicalCommitRequest` with `live_promotions: Vec<CanonicalLivePromotionMutation>` and `CanonicalRuntimeStore` with `fn audit_entry_by_id(&self, audit_id: &str) -> io::Result<Option<CanonicalAuditEntry>>`. File/fake/PostgreSQL adapters implement both; only PostgreSQL accepts a live-promotion mutation, while the fake supports deterministic tests.
- Generic `deployments resume` rejects non-archived live deployments. The only live transition to Running is the Admin-only `POST /api/live-promotion/approve-and-resume` operation; paper/dry-run resume behavior is unchanged.
- The request uses strict generated contracts and the full parity fields defined by the later Agent Task 2 producer: candidate/dry-run/expected-live-config/model/runner/recording hashes, `live_config_materialized=true`, horizon/symbol/profile/runtime score, executable cost, average entry, maximum drawdown, bankroll/cost assumptions, strict parity flag, source workflow/run/main SHA, artifact name/hash, and zero blockers. Missing/unknown/mismatched fields fail before state mutation.
- `ProtectedWorkflowProvenance` is signed over only its `unsigned` member using RFC 8785 JSON Canonicalization Scheme bytes; `hmac_sha256` is structurally excluded. The unsigned payload covers canonical approval SHA, parity content/artifact SHA, expected live config SHA, RDS proof, release SHA, the exact emergency-stop and restart-recovery audit IDs, nonce, and time. The daemon requires `request.emergency_stop_audit_id == provenance.unsigned.emergency_stop_audit_id` and the same byte-for-byte equality for `restart_recovery_audit_id` before looking up either audit; no unsigned top-level alias is authoritative. Rust and the later workflow Python helper share committed cross-language test vectors for nested RDS proof/timestamps/audit IDs and reject duplicate/non-canonical fields. Daemon config requires a distinct `PLOY_LIVE_GATE_HMAC_KEY`; verify HMAC-SHA256 in constant time, exact workflow name, exact current release/main SHA, artifact SHA, signed audit IDs, nonce, and an issuance time no older than 15 minutes. Caller-provided hash without a valid signature is untrusted. Neither script nor Agent can mint this receipt.
- `HumanLiveApproval` requires a unique approval ID, reviewer identity, approval/expiry times (maximum 24 hours), exact normalized account, USD 5 cap, candidate/parity/config/model/runner hashes, and exact acknowledgement. Validate it independently from parity/provenance.
- Migration 050 stores every field deterministically available from `CanonicalLivePromotionRecord`: approval ID primary key, unique `provenance.unsigned.nonce`, normalized account, release/head SHA, parity content/artifact/RDS proof/config/model/runner hashes, request SHA, status, typed result JSON, and created/updated timestamps. The adapter never receives an untyped partial row. Insert/consume/update it through the same canonical PostgreSQL transaction/store; restart cannot reuse an ID or nonce. File canonical mode rejects the operation.
- Before consuming approval, prove canonical PostgreSQL health, paused unrendered-to-rendered manifest identity, one wallet/cap, authenticated V2+geoblock health, collateral balance and allowance each at least USD 5, fresh venue heartbeat, zero unknown/unresolved/open orders or unexplained positions, and exact successful emergency-stop plus restart-recovery audit IDs from the canonical store. All gateway calls share the caller's pre-lock absolute deadline.
- Add Admin-only `POST /api/system/recovery-proof` and `ployctl system record-recovery-proof --emergency-audit-id <id>`. It is callable only after a real process restart: compare the persisted emergency receipt's daemon boot ID with the current boot ID, require they differ, then prove zero unresolved state and fresh authenticated venue health before writing a typed canonical `RestartRecoveryReceipt`. A caller-supplied string or same-boot receipt cannot satisfy the promotion gate.
- Commit the consumed approval, desired/observed transition, every affected deployment version, and canonical audit atomically. Start the one allowlisted worker, reprobe, and return success only for desired `Running`, observed `Running`, fresh venue truth, and the exact USD 5 FAK profile. Any worker/probe/post-commit failure performs a compensating canonical pause/degrade transaction and returns non-success; it never retries approval or starts another strategy.
- `ployctl system approve-live-canary --request <path>` calls this endpoint and preserves typed 200/409 error bodies. The hardened shell gate invokes only this command; it never calls `deployments resume`.
- Local tests use fake canonical store/gateway/HMAC fixtures and no real wallet, GitHub, database, or venue. The protected workflow and real parity producer land later; until then the operation is cryptographically unreachable in production configuration.

Required tests:

```text
generic_resume_rejects_live_but_keeps_paper_behavior
live_promotion_requires_admin_canonical_postgres_and_valid_hmac_provenance
caller_supplied_artifact_sha_without_signature_is_rejected
signed_recovery_audit_ids_must_equal_request_and_swapped_ids_are_rejected
approval_id_and_nonce_cannot_replay_across_restart
parity_candidate_config_model_runner_recording_and_horizon_must_match
balance_allowance_geoblock_health_unknown_state_and_recovery_receipts_fail_closed
restart_recovery_receipt_requires_different_boot_id_and_fresh_zero_state
successful_usd5_fak_promotion_commits_versions_audit_and_running_running_state
worker_or_post_commit_probe_failure_compensates_to_paused_degraded
shell_gate_has_no_direct_deployments_resume_path
every_gateway_fake_declares_account_readiness_and_shared_deadline_behavior
```

Documentation must state:

- V2 execution is official SDK 0.6.0 and V2 host only.
- Market resolution, `redeemable`, closed position, and confirmed redeem receipt are distinct evidence.
- Failed/reverted/timed-out redemption is a persisted retryable degraded state; a later matching confirmation releases quantity once.
- Official closed CLOB+Gamma resolution proves payout `0`, `0.5`, or `1`; REDEEM `usdc_size` only corroborates `quantity * official_payout`, and the trade-only activity price field is never used.
- Ploy closes canonical quantity only on confirmed transaction/relayer evidence.
- PostgreSQL is canonical for production deployment/trading/audit state; JSON files are post-commit caches in that mode, and file mode cannot host live state.
- Auto-claim/Data API reconciliation is the default account lifecycle.
- Manual redeem remains absent and cannot be called by a strategy worker or Agent.
- If remote evidence later proves manual redeem is required, create a separate `account-ops` issue/PR with Admin-only command, non-default `ctf` feature, idempotency, relayer-first policy, and post-transaction confirmation through the same reconcile path.

Run:

```bash
cargo run -p ploy-operator-contracts --example export_schemas
node scripts/export_operator_contract_types.mjs
rtk cargo fmt --all -- --check
rtk cargo test --locked \
  -p ploy-trading \
  -p ploy-connectivity \
  -p ploy-operator-contracts \
  -p ploy-platform-runtime \
  -p ploy-daemon-host \
  -p ployctl
rtk cargo check --locked -p new-ployd
rtk pytest tests/test_polymarket_v2_execution_contracts.py \
  tests/test_canonical_runtime_store_contracts.py \
  tests/test_live_promotion_contracts.py \
  tests/test_live_promotion_gate.py
scripts/check_v2_claim_redeem_gate.sh
rtk cargo test --locked --workspace
rtk git diff --check
```

Expected result: all commands pass, the vendored package is not a workspace test target, and no real API/signing/account operation is executed.

Commit:

```bash
git add crates/ploy-connectivity/Cargo.toml \
  crates/ploy-connectivity/src/lib.rs \
  crates/ploy-platform-runtime/src/trade_submit.rs \
  crates/ploy-platform-runtime/src/trade_control.rs \
  apps/new-ployd/tests/sigterm.rs \
  crates/ploy-operator-contracts/src/system.rs \
  crates/ploy-operator-contracts/src/lib.rs \
  crates/ploy-operator-contracts/src/schemas.rs \
  contracts/schemas/recorded-runtime-parity-v2.schema.json \
  contracts/schemas/restart-recovery-receipt.schema.json \
  contracts/schemas/live-promotion-request.schema.json \
  contracts/schemas/live-promotion-response.schema.json \
  ploy-frontend/src/types/operator-contracts.ts \
  ploy-sidecar/src/contracts/operator-contracts.ts \
  migrations/050_live_promotion_approvals.sql \
  crates/ploy-daemon-host/src/canonical_store.rs \
  crates/ploy-daemon-host/src/config.rs \
  crates/ploy-daemon-host/src/runtime.rs \
  crates/ploy-daemon-host/src/http.rs \
  crates/ploy-control-client/src/lib.rs \
  apps/ployctl/src/system.rs \
  apps/ployctl/src/main.rs \
  scripts/validate_live_promotion_gate.py \
  scripts/drills/pm5d_threelayer_live_gate.sh \
  tests/test_live_promotion_gate.py \
  tests/test_live_promotion_contracts.py \
  docs/operations/v2-claim-redeem-gate.md \
  docs/runbooks/live-deployment-checklist.md \
  tasks/todo.md
git commit -m "feat(safety): add canonical live promotion operation"
```

## Completion Criteria

- Live execution defaults to official CLOB V2 and rejects V1 protocol.
- AWS KMS stays only an upstream optional/example concern, not a Ploy normal dependency or workspace test target.
- Domain/runtime crates expose only Ploy types.
- Resolution and redemption remain separate transitions.
- Failed/reverted/unconfirmed redemption retains canonical position/exposure.
- Confirmed payouts `0`, `0.5`, and `1` close quantity and realize PnL exactly once without a SELL fill.
- Trading-state restart preserves settlement history and idempotency.
- Manual redeem is not implemented or enabled.
- Generic live resume is disabled; only a signed, canonical, single-use USD 5 promotion request can reach Running, and the protected workflow remains absent/disabled until later packaging.
- No live credential, order, or redemption was used locally.
