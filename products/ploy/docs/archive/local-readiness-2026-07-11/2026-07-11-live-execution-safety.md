# Live Execution Safety Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans task-by-task.

Goal: Make every live risk increase, order lifetime, venue-health claim, and shutdown transition fail closed before any Polymarket V2 or cloud deployment work begins.

Architecture: Keep one daemon-owned live gateway and one shared admission/quiesce path. Derive risk effect from the current canonical position, require fresh venue evidence for risk increase, use FAK for the initial canary, and treat venue-confirmed state as the only authority for cancellation.

Tech Stack: Rust 1.91, Tokio, Serde, rust_decimal, existing `ployd`/`ployctl` control-plane contracts, Bash, and the currently integrated Polymarket adapter. The official SDK migration is handled by the next plan.

## Global Constraints

- Do not enable, resume, fund, or deploy a live strategy.
- Do not use a real private key or make a real authenticated venue call in tests.
- Do not run local PostgreSQL.
- Keep `config/deployments/pm5d.threelayer.live.json` paused and intentionally unrendered until an operator supplies the actual normalized wallet address.
- `ployd` remains the sole owner of live gateway credentials and state mutation.
- Every money/security change begins with a failing regression test.
- Each task is one atomic commit and stages only the listed paths.
- Tasks 1-2 belong to branch/PR `fix/live-admission-account-guards`. After it merges, Tasks 3-7 run on `fix/live-order-quiesce` from updated `main`.

---

### Task 1: Classify intent risk and require fresh venue evidence

Files:

- Modify `crates/ploy-platform/src/system.rs`.
- Modify `crates/ploy-platform-runtime/src/runtime_support.rs`.
- Modify `crates/ploy-platform-runtime/src/deployment_control.rs`.
- Modify `crates/ploy-daemon-host/src/runtime.rs`.
- Modify `crates/ploy-daemon-host/src/config.rs`.
- Modify `crates/ploy-daemon-host/src/http.rs`.
- Modify `crates/ploy-control-client/src/lib.rs`.
- Modify `crates/ploy-strategy-runtime/src/live.rs`.
- Modify focused tests in those files.

Interfaces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentRiskEffect {
    Increase,
    Reduce,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentAdmissionSource {
    Worker,
    AuthenticatedOperator,
    Emergency,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenExposureEnvelope {
    pub settled_net_qty: Decimal,
    pub worst_case_min_qty: Decimal,
    pub worst_case_max_qty: Decimal,
}

pub fn account_token_exposure_envelope(
    deployments: &[DeploymentRecord],
    trading: &BTreeMap<String, TradingRuntime>,
    account_id: &str,
    token_id: &str,
) -> TokenExposureEnvelope;

pub fn intent_risk_effect(
    intent: &TradingIntent,
    exposure: TokenExposureEnvelope,
) -> IntentRiskEffect;

pub fn ensure_intent_allowed(
    deployment: &DeploymentRecord,
    intent: &TradingIntent,
    risk_effect: IntentRiskEffect,
    venue_health_fresh: bool,
    source: IntentAdmissionSource,
) -> io::Result<()>;

impl PloyDaemon {
    pub fn submit_intent_idempotent_from(
        &mut self,
        intent: TradingIntent,
        idempotency_key: Option<&str>,
        source: IntentAdmissionSource,
    ) -> io::Result<PaperIntentResponse>;
}
```

Add an evidence-time helper to `SystemService` so absence is not mistaken for health:

```rust
pub fn source_is_fresh_at(
    &self,
    source_id: &str,
    now: DateTime<Utc>,
) -> bool;
```

Rules:

- `Entry` is always `Increase`.
- Build the account+token envelope from settled position plus the remaining quantity of every acknowledged, active, partially filled, or unknown canonical order across deployments for the same normalized wallet. Sells extend the minimum bound; buys extend the maximum bound.
- `Hedge` compares `max(abs(min), abs(max))` before and after adding its signed quantity. It is `Reduce` only when the post-intent worst-case bound is strictly smaller; otherwise it is `Increase`. Two stacked opposite hedges therefore cannot both be classified as reductions that later flip the position.
- `Reduce` and `Exit` are `Reduce`; `Cancel` is `Control`.
- Live `Increase` requires deployment state `Enabled`, desired `Running`, observed `Running`, and `source_is_fresh_at("venue:polymarket", now) == true`.
- Starting, degraded, draining, recovering, paused, stopped, failed, disabled, and archived live deployments reject risk increase.
- Degraded/draining/recovering live deployments permit only risk reduction and control.
- Desired paused, stopped, or failed permits risk reduction only for `AuthenticatedOperator` or `Emergency`; a `Worker` call is rejected even if a worker process is accidentally still alive.
- Paper mode keeps its existing lifecycle behavior and does not require venue health.
- Add a distinct `PLOY_WORKER_TOKEN`, `AuthLevel::Worker`, and `x-ploy-worker-token` header. The strategy runtime constructs a worker-scoped control client; operator/admin tokens continue to identify explicit operator calls. The worker token grants only the intent endpoint and no deployment/system/order-control endpoint.
- HTTP derives `IntentAdmissionSource` from the authenticated principal and passes it into `PloyDaemon::submit_intent_idempotent_from`; callers cannot choose the source in JSON. Both worker and operator intent paths still share this one daemon matrix.
- When live auth is configured, a missing worker/operator identity fails closed. Paper-only compatibility tests may keep the existing no-auth local mode.

Step 1: Add failing tests.

```text
absent_venue_source_is_not_fresh
stale_venue_source_is_not_fresh_without_refresh_side_effect
degraded_live_rejects_entry_and_increasing_hedge_but_allows_reduction
starting_live_rejects_risk_increase
fresh_running_live_allows_risk_increase
draining_live_rejects_increasing_hedge
stacked_hedges_use_worst_case_active_and_unknown_order_exposure
paused_live_reduction_rejects_worker_but_accepts_operator
worker_token_cannot_access_operator_or_admin_endpoints
intent_json_cannot_spoof_admission_source
```

Step 2: Run the RED tests.

```bash
rtk cargo test -p ploy-platform absent_venue_source_is_not_fresh --lib
rtk cargo test -p ploy-platform-runtime degraded_live_rejects_entry_and_increasing_hedge_but_allows_reduction --lib
rtk cargo test -p ploy-daemon-host fresh_running_live_allows_risk_increase --lib
```

Expected RED result: the freshness helper/signature does not exist and the current gate permits observed-degraded or no-probe live intent paths.

Step 3: Implement the minimal shared matrix.

- Collect `DeploymentRegistry::records()` and compute the account+token exposure envelope from every matching non-archived deployment runtime before exposure reservation and while holding the existing daemon mutation lock. Missing runtime state for a matching live deployment is an admission error, not zero exposure.
- Call `refresh_source_health()` before admission, then evaluate `source_is_fresh_at` directly against the stored `last_seen_at` and `stale_after`.
- Treat a missing source entry, missing `last_seen_at`, forced-stale source, or expired timestamp as false.
- Update existing `intent_allowed_while_draining()` so it no longer grants all hedges.

Step 4: Run the focused and crate suites.

```bash
rtk cargo test -p ploy-platform --lib
rtk cargo test -p ploy-platform-runtime --lib
rtk cargo test -p ploy-daemon-host --lib
rtk git diff --check
```

Expected GREEN result: the full admission matrix passes and no alternate submission path bypasses it.

Step 5: Commit.

```bash
git add crates/ploy-platform/src/system.rs \
  crates/ploy-platform-runtime/src/runtime_support.rs \
  crates/ploy-platform-runtime/src/deployment_control.rs \
  crates/ploy-daemon-host/src/runtime.rs \
  crates/ploy-daemon-host/src/config.rs \
  crates/ploy-daemon-host/src/http.rs \
  crates/ploy-control-client/src/lib.rs \
  crates/ploy-strategy-runtime/src/live.rs
git commit -m "fix(risk): fail closed on live admission health"
```

---

### Task 2: Enforce canonical live wallet and account cap scope

Files:

- Modify `crates/ploy-platform-runtime/src/deployment_control.rs`.
- Modify `crates/ploy-platform-runtime/src/bootstrap.rs`.
- Modify `crates/ploy-daemon-host/src/runtime.rs`.
- Modify every JSON file under `config/deployments/` with `runtime_mode=paper`.
- Modify `config/deployments/pm5d.threelayer.live.json`.
- Modify `config/strategies/02-pm5d-threelayer.live.toml`.
- Modify `scripts/drills/pm5d_threelayer_live_gate.sh`.
- Modify `tests/test_strategy_config_contracts.py`.
- Modify focused Rust tests that construct deployment records.

Interfaces:

```rust
pub fn normalize_live_account_id(raw: &str) -> io::Result<String>;

pub fn validate_live_account_scope(
    candidate: &DeploymentRecord,
    existing: &[DeploymentRecord],
) -> io::Result<()>;
```

Validation rules:

- Live IDs are lowercase `0x` plus exactly 40 hexadecimal characters.
- Reject the all-zero address.
- Paper IDs start with `paper:` and cannot be valid live wallet IDs.
- Every non-archived live deployment has `max_gross_exposure > 0`.
- All non-archived live deployments in one daemon use the same normalized wallet and exactly the same account cap.
- Ignore the old record with the same `deployment_id` during reapply validation.
- Unsafe restored live records are persisted as desired `Paused`, observed `Degraded`, and their worker is not started.
- Account exposure aggregation remains in the existing daemon method; do not introduce an account manager.

Committed manifest policy:

- Change paper manifests from `acct-*` to stable `paper:*` IDs.
- Keep the live manifest paused with `account_id` set to the explicit non-address sentinel `live-wallet-must-be-rendered`.
- Change the live strategy fixed stake to `5.0` so it can fit the later USD 5 cap.
- Set the live strategy `allowed_window_secs = [300]`. The live profile is PM5D-only; `[300, 900]` is an invalid cross-horizon capability declaration and the horizon-safe slice must not need to repair it later.
- The committed sentinel must fail `apply_deployment` and may never be copied into canonical runtime state.
- `pm5d_threelayer_live_gate.sh` requires `PLOY_LIVE_ACCOUNT_ID`, validates it, renders a temporary manifest with the normalized address, and applies only that temporary file. It never rewrites the repository file and never prints the address alongside secret material.

Step 1: Add failing tests.

Rust tests:

```rust
fn live_deployment_requires_normalized_nonzero_wallet()
fn live_deployment_requires_positive_cap()
fn paper_account_requires_paper_namespace()
fn live_deployments_require_one_wallet_and_equal_cap()
fn loaded_unsafe_live_account_scope_is_paused_degraded()
fn concurrent_account_submissions_cannot_exceed_cap()
```

Python/shell contract tests:

```text
test_live_manifest_is_paused_and_unrendered
test_live_fixed_stake_does_not_exceed_cap
test_live_profile_declares_exactly_pm5d_window
test_live_gate_requires_and_normalizes_wallet
test_paper_manifests_use_paper_namespace
```

Step 2: Run the RED tests.

```bash
rtk cargo test -p ploy-platform-runtime live_deployment_requires_normalized_nonzero_wallet --lib
rtk cargo test -p ploy-platform-runtime loaded_unsafe_live_account_scope_is_paused_degraded --lib
rtk cargo test -p ploy-daemon-host concurrent_account_submissions_cannot_exceed_cap --lib
rtk pytest tests/test_strategy_config_contracts.py
```

Expected RED result: aliases are accepted as live account IDs, paper IDs lack the namespace, and live stake exceeds its cap.

Step 3: Implement and update fixtures.

- Implement normalization without adding Alloy to `ploy-platform-runtime`.
- Call validation from both `apply_deployment()` and `apply_loaded_registry_state()`.
- Update test records mechanically to use `paper:test-*` or a normalized non-zero live address according to mode.
- Keep the existing persistence-before-live-submit and account-level serialization behavior unchanged.

Step 4: Verify.

```bash
rtk cargo test -p ploy-platform-runtime --lib
rtk cargo test -p ploy-daemon-host --lib
rtk pytest tests/test_strategy_config_contracts.py
bash -n scripts/drills/pm5d_threelayer_live_gate.sh
rg -n '"runtime_mode": "paper"' config/deployments
if rg -n '"account_id": "acct-' config/deployments; then
  echo "legacy paper account alias remains" >&2
  exit 1
fi
rtk git diff --check
```

Expected result: the final `rg` for `acct-` exits with no matches; the live manifest stays paused and is rejected until rendered with an explicit wallet.

Step 5: Commit.

```bash
git add crates/ploy-platform-runtime/src/deployment_control.rs \
  crates/ploy-platform-runtime/src/bootstrap.rs \
  crates/ploy-daemon-host/src/runtime.rs \
  config/deployments \
  config/strategies/02-pm5d-threelayer.live.toml \
  scripts/drills/pm5d_threelayer_live_gate.sh \
  tests/test_strategy_config_contracts.py
git commit -m "fix(accounts): enforce live wallet and cap scope"
```

---

### Task 3: Add FAK, cancel-all, open-order, and health gateway contracts

Files:

- Modify `crates/ploy-connectivity/src/lib.rs`.
- Modify `crates/ploy-platform-runtime/src/trade_submit.rs`.
- Modify `crates/ploy-platform-runtime/src/trade_control.rs`.
- Modify `crates/ploy-daemon-host/src/runtime.rs`.
- Modify `crates/ploy-daemon-host/src/http.rs`.

Interfaces:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueOpenOrder {
    pub venue_order_id: String,
    pub token_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenuePosition {
    pub condition_id: String,
    pub token_id: String,
    pub quantity: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueResidualState {
    Filled,
    Canceled,
    Open,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueSettlementProtocol {
    Binary,
    NegRisk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueSettlementIdentity {
    pub condition_id: String,
    pub event_id: String,
    pub token_id: String,
    pub protocol: VenueSettlementProtocol,
    pub collateral_token: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionOutcome {
    Acknowledged {
        venue_order_id: String,
        immediate_fills: Vec<FillRecord>,
        residual_state: VenueResidualState,
        post_submit_error: Option<String>,
        settlement_identity: Option<VenueSettlementIdentity>,
    },
    Rejected { reason: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BulkCancellationOutcome {
    pub canceled: Vec<String>,
    pub not_canceled: BTreeMap<String, String>,
}

pub trait LiveExecutionGateway: Send + Sync + Debug {
    fn submit(
        &self,
        request: &ExecutionRequest,
        deadline: Instant,
    ) -> Result<ExecutionOutcome, ExecutionError>;
    fn cancel(
        &self,
        request: &CancellationRequest,
        deadline: Instant,
    ) -> Result<CancellationOutcome, ExecutionError>;
    fn replace(
        &self,
        request: &ReplaceRequest,
        deadline: Instant,
    ) -> Result<ReplaceOutcome, ExecutionError>;
    fn cancel_all(&self, deadline: Instant) -> Result<BulkCancellationOutcome, ExecutionError>;
    fn list_open_orders(&self, deadline: Instant) -> Result<Vec<VenueOpenOrder>, ExecutionError>;
    fn list_positions(&self, deadline: Instant) -> Result<Vec<VenuePosition>, ExecutionError>;
    fn health_check(&self, deadline: Instant) -> Result<(), ExecutionError>;
    fn reconcile_fills(
        &self,
        tracked_orders: &[TrackedOrder],
        deadline: Instant,
    ) -> Result<Vec<FillRecord>, ExecutionError>;
}

impl PloyDaemon {
    pub fn submit_intent_idempotent_from_until(
        &mut self,
        intent: TradingIntent,
        idempotency_key: Option<&str>,
        source: IntentAdmissionSource,
        deadline: Instant,
    ) -> io::Result<PaperIntentResponse>;

    pub fn cancel_order_until(
        &mut self,
        deployment_id: &str,
        order_id: &str,
        deadline: Instant,
    ) -> io::Result<OrderControlResponse>;

    pub fn replace_order_until(
        &mut self,
        deployment_id: &str,
        order_id: &str,
        request: OrderReplaceRequest,
        deadline: Instant,
    ) -> io::Result<OrderControlResponse>;
}
```

Add deterministic fake builders:

```rust
pub fn with_cancel_all_result(
    self,
    result: Result<BulkCancellationOutcome, ExecutionError>,
) -> Self;

pub fn with_open_orders_result(
    self,
    result: Result<Vec<VenueOpenOrder>, ExecutionError>,
) -> Self;

pub fn with_positions_result(
    self,
    result: Result<Vec<VenuePosition>, ExecutionError>,
) -> Self;

pub fn with_health_result(
    self,
    result: Result<(), ExecutionError>,
) -> Self;
```

Rules:

- `finish_live_intent()` sends `OrderExecutionType::FAK` for the initial live entry/hedge path.
- GTC remains in the domain enum for a future declared resting-order strategy but is not selected by the initial profile.
- `PolymarketExecutionGateway::health_check()` performs both a public server-time request and an authenticated open-orders request.
- The combined probe succeeds only when both calls succeed under the same deadline. Invalid credentials and authenticated-call timeout are distinct errors; neither is converted to public-only health or refreshes the venue heartbeat.
- `list_open_orders()` paginates until the terminal cursor; one page is not account truth.
- `list_positions()` fetches the normalized account's positive venue positions through an account-scoped, paginated, mockable data surface. The legacy adapter may fail this call as unsupported until the V2 plan replaces it, but it may not return an empty success by default.
- `cancel_all()` maps the venue response without inferring cancellation for IDs absent from `canceled`.
- Every gateway method that can reach the venue, including submit, cancel, replace, post-submit lookup, auth recovery, and every read/reconcile method, receives one absolute monotonic deadline. No no-deadline live mutation method remains on the trait. Ordinary submit/cancel/replace and reconciliation entrypoints construct one 5,000 ms deadline before acquiring the daemon mutation path and pass that same value through every SDK/auth/follow-up call; emergency quiesce passes its already-established shared deadline. Bound each network operation with `min(deadline - Instant::now(), venue_timeout_ms)` on the existing gateway Tokio runtime; an expired budget returns a timeout without making another network call or retrying authentication.
- HTTP/control callers create the ordinary 5,000 ms absolute deadline before attempting to acquire the daemon mutex and call the `_until` submit/cancel/replace form. Lock wait therefore consumes the same budget as admission, auth, signing, mutation, follow-up lookup, and persistence. Compatibility wrappers may create a deadline for direct paper/test callers, but no live HTTP/control path may reset it after locking.
- After a successful venue mutation, the adapter immediately performs the bounded order/trade lookup needed to return every currently visible fill and a residual state. A failure in this post-submit lookup returns an acknowledged outcome with `residual_state=Unknown` and `post_submit_error=Some(error_message)`, never a transport `Err`, because the order may already exist at the venue.
- `finish_live_intent()` acknowledges the order, validates and applies each `immediate_fills` record idempotently, then applies `Filled`/`Canceled` terminal state without erasing partial fills. `Open` is invalid for the initial FAK/FOK profile; `Open` or `Unknown` leaves the order unresolved.
- `PloyDaemon::submit_live_intent()` persists the pending order before mutation and persists the acknowledgement, immediate fills, positions/PnL, and residual state before returning the HTTP response. A persistence failure returns no success response, records a critical persistence-source alert, pauses/degrades the deployment, and requires reconciliation before any resume/retry. An unresolved post-submit state also pauses/degrades and requires reconciliation.
- The legacy adapter may return `settlement_identity=None`, which keeps settlement readiness degraded. The V2 plan must resolve and persist a complete token/condition/event/protocol/collateral identity at order time before live readiness can pass; settlement reconciliation never guesses identity from a redeem DTO.

Step 1: Add failing tests.

```rust
fn live_submit_uses_fak()
fn gateway_health_requires_public_and_authenticated_calls()
fn gateway_health_rejects_invalid_authenticated_response()
fn authenticated_health_timeout_is_transport_error()
fn list_open_orders_paginates_to_terminal_cursor()
fn list_positions_paginates_and_rejects_wrong_account_rows()
fn cancel_all_preserves_partial_failure_map()
fn gateway_timeout_is_transport_error()
fn submit_cancel_and_replace_share_one_absolute_deadline_with_followups()
fn expired_mutation_deadline_makes_no_sdk_or_auth_call()
fn http_constructs_live_mutation_deadline_before_daemon_lock()
fn lock_wait_consumes_submit_cancel_and_replace_budget()
fn fak_partial_fill_returns_fill_and_canceled_residual()
fn accepted_order_with_failed_followup_is_unknown_not_transport_rejected()
fn daemon_persists_partial_fill_before_submit_response()
fn partial_fill_persistence_failure_alerts_pauses_degrades_and_returns_error()
fn unknown_submit_outcome_remains_paused_until_reconciled()
```

Use a local mock HTTP listener for adapter tests. Never call the public venue in the test suite.

Step 2: Run RED.

```bash
rtk cargo test -p ploy-platform-runtime live_submit_uses_fak --lib
rtk cargo test -p ploy-connectivity gateway_health_requires_public_and_authenticated_calls --lib
rtk cargo test -p ploy-connectivity cancel_all_preserves_partial_failure_map --lib
```

Expected RED result: submit still uses GTC and the gateway trait lacks the new operations.

Step 3: Implement the narrow gateway extension.

- Reuse the current authenticated client cache and auth-recovery behavior.
- Do not expose SDK request/response types outside `ploy-connectivity`.
- Ensure every fake implementation makes its health/open-order behavior explicit; do not add default trait no-ops.
- Re-run `rg -l 'impl LiveExecutionGateway' --glob '*.rs'` immediately before editing and update every result. At the approved-plan base these are exactly connectivity, `trade_submit.rs`, `trade_control.rs`, and daemon runtime tests; a newly added implementation must be included rather than receiving default no-op behavior.

Step 4: Verify.

```bash
rtk cargo test -p ploy-connectivity --lib
rtk cargo test -p ploy-platform-runtime trade_submit --lib
rtk cargo check -p ploy-daemon-host
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-connectivity/src/lib.rs \
  crates/ploy-platform-runtime/src/trade_submit.rs \
  crates/ploy-platform-runtime/src/trade_control.rs \
  crates/ploy-daemon-host/src/http.rs \
  crates/ploy-daemon-host/src/runtime.rs
git commit -m "fix(execution): use FAK and expose venue truth"
```

---

### Task 4: Preserve unknown venue state and probe the idle venue

Files:

- Modify `crates/ploy-platform-runtime/src/trade_control.rs`.
- Modify `crates/ploy-platform-runtime/src/reconcile.rs`.
- Modify `crates/ploy-platform-runtime/src/health_runtime.rs`.
- Modify `crates/ploy-daemon-host/src/runtime.rs`.

Interfaces:

```rust
pub fn reconcile_live_fills(
    gateway: &dyn LiveExecutionGateway,
    deployments: &[DeploymentRecord],
    trading: &mut BTreeMap<String, TradingRuntime>,
) -> io::Result<ReconcileStatus>;

pub fn reconcile_live_fills_until(
    gateway: &dyn LiveExecutionGateway,
    deployments: &[DeploymentRecord],
    trading: &mut BTreeMap<String, TradingRuntime>,
    deadline: Instant,
) -> io::Result<ReconcileStatus>;
```

The no-deadline wrapper calls `reconcile_live_fills_until` with the ordinary 5,000 ms budget. Emergency quiesce calls the `until` form with its already-established shared deadline. Keep `ReconcileStatus` for this slice; the V2 settlement plan expands its counts later.

Rules:

- Live cancel without `venue_order_id` calls `mark_order_unknown`, sets a concrete last error, returns an incomplete response, and never calls `cancel_order()` locally.
- A venue rejection or transport error preserves the active/unknown state and last error.
- If any non-archived live deployment exists, reconciliation calls `health_check()` before it may return `Noop`, including zero tracked orders.
- Boot does not call `mark_runtime_healthy()` until a successful real probe/reconcile result.
- Failed probe marks live deployments observed `Degraded`, leaves the old heartbeat stale, and closes admission.
- A successful probe clears backoff and may restore desired-running live deployments to observed `Running`.
- Unrecognized venue open orders make readiness degraded; do not import or cancel them automatically in this task.
- Reconciliation also calls `list_positions()` for the one normalized live account and compares positive venue quantities with canonical positions aggregated across all non-archived live deployments. An unknown condition/token, an excess venue quantity, or any unexplained quantity mismatch makes readiness degraded. Do not import, sell, or redeem it automatically.
- A position-list failure is a venue-truth failure and does not refresh the health timestamp. The V2 settlement plan later explains known reductions through confirmed redemption records; until then, a mismatch remains fail-closed.
- Any canonical order in `Unknown`, including one without a venue order ID after a transport-ambiguous submit, blocks readiness and a transition back to desired `Running`. This slice clears it only through venue reconciliation; a future Admin explicit-resolution operation would be a separate audited feature. Repeating the original idempotency key never resubmits it.

Step 1: Add failing tests.

```rust
fn live_cancel_without_venue_id_stays_unknown()
fn live_cancel_rejection_preserves_unresolved_order()
fn idle_live_reconcile_still_probes_venue()
fn idle_live_probe_failure_returns_error()
fn daemon_idle_probe_failure_marks_live_degraded()
fn boot_does_not_mark_live_healthy_before_successful_probe()
fn invalid_auth_probe_does_not_refresh_heartbeat()
fn authenticated_probe_timeout_does_not_refresh_heartbeat()
fn unrecognized_venue_order_blocks_readiness()
fn unrecognized_venue_position_blocks_readiness()
fn venue_position_quantity_mismatch_blocks_readiness()
fn unresolved_canonical_order_blocks_resume_and_retry_until_reconciled()
fn idempotent_retry_of_unknown_order_never_calls_submit_again()
```

Step 2: Run RED.

```bash
rtk cargo test -p ploy-platform-runtime live_cancel_without_venue_id_stays_unknown --lib
rtk cargo test -p ploy-platform-runtime idle_live_reconcile_still_probes_venue --lib
rtk cargo test -p ploy-daemon-host boot_does_not_mark_live_healthy_before_successful_probe --lib
```

Expected RED result: missing venue ID is locally canceled and idle reconcile never calls the venue.

Step 3: Implement without a second health service.

- Reuse current `LiveHealthConfig`, source heartbeat, backoff, and observed-state transitions.
- Remove the unconditional healthy mark from `boot_with_live_execution()`.
- Compare `list_open_orders()` output with canonical active venue IDs and `list_positions()` with aggregate canonical positions; return a concrete error listing unknown IDs and mismatched token quantities.

Step 4: Verify.

```bash
rtk cargo test -p ploy-platform-runtime trade_control --lib
rtk cargo test -p ploy-platform-runtime reconcile --lib
rtk cargo test -p ploy-daemon-host daemon_idle_probe_failure_marks_live_degraded --lib
rtk cargo test -p ploy-daemon-host daemon_surfaces_transient_reconcile_failures_as_degraded_then_recovering --lib
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-platform-runtime/src/trade_control.rs \
  crates/ploy-platform-runtime/src/reconcile.rs \
  crates/ploy-platform-runtime/src/health_runtime.rs \
  crates/ploy-daemon-host/src/runtime.rs
git commit -m "fix(reconcile): keep unresolved venue state fail closed"
```

---

### Task 5: Implement the daemon-owned emergency quiesce transaction

Files:

- Modify `crates/ploy-operator-contracts/src/system.rs`.
- Modify `crates/ploy-operator-contracts/src/lib.rs`.
- Modify `crates/ploy-operator-contracts/src/schemas.rs`.
- Add the generated `contracts/schemas/emergency-stop-response.schema.json` through the existing exporter.
- Modify `crates/ploy-daemon-host/src/runtime.rs`.
- Add `crates/ploy-daemon-host/src/audit_io.rs`.
- Modify `crates/ploy-daemon-host/src/lib.rs`.
- Modify `crates/ploy-daemon-host/src/http.rs` to reuse `audit_io`.

Contracts:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnresolvedLiveOrder {
    pub deployment_id: String,
    pub client_order_id: String,
    pub venue_order_id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnresolvedLivePosition {
    pub account_id: String,
    pub condition_id: Option<String>,
    pub token_id: Option<String>,
    pub venue_quantity: Option<Decimal>,
    pub canonical_quantity: Option<Decimal>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EmergencyStopResponse {
    pub success: bool,
    pub daemon_boot_id: String,
    pub paused_deployments: Vec<String>,
    pub stopped_workers: Vec<String>,
    pub canceled_venue_order_ids: Vec<String>,
    pub remaining_venue_order_ids: Vec<String>,
    pub unresolved_orders: Vec<UnresolvedLiveOrder>,
    pub unresolved_positions: Vec<UnresolvedLivePosition>,
    pub venue_errors: Vec<String>,
    pub critical_alerts: Vec<ActiveAlert>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}
```

Daemon interface:

```rust
impl PloyDaemon {
    pub fn cancel_all_live_orders_until(
        &mut self,
        deadline: Instant,
    ) -> Result<BulkCancellationOutcome, ExecutionError>;

    pub fn emergency_stop_until(
        &mut self,
        deadline: Instant,
    ) -> io::Result<EmergencyStopResponse>;
}
```

Fixed operation order:

1. Select all non-archived live deployments under the daemon mutation lock. If there are no live deployments and no live canonical ledgers, persist a deterministic empty success receipt and return without constructing or calling the gateway.
2. Set desired `Paused` and observed `Draining`.
3. Persist the registry; abort before venue mutation if this write fails.
4. Stop the corresponding workers through `WorkerSupervisor::stop`.
5. Call account-wide `cancel_all(deadline)` once.
6. Mark only venue-confirmed canceled canonical orders as canceled.
7. Call `list_open_orders(deadline)`, `list_positions(deadline)`, and `reconcile_live_fills_until(gateway, &live_deployments, &mut self.trading, deadline)` while budget remains.
8. Build unresolved-order entries for partial cancel failures, missing venue IDs, and canonical active orders still reported by the venue. Build unresolved-position entries for list failure, unknown condition/token, and venue/canonical quantity mismatch. For every incomplete venue/account result, call the existing `SystemService::note_source_failure` with source kind `venue`, materialize the resulting `AlertSeverity::Critical` alert, and include it in the response.
9. Persist trading state, registry final observed states, the status/runtime snapshots, and an audit JSONL entry whose message is the serialized response including the full critical-alert records. This audit record is the durable alert receipt; `/api/system/alerts` remains the active in-memory projection.
10. Return `success=true` only when remaining venue orders, unresolved canonical orders, and unresolved venue/canonical positions are all empty.

Error accumulation rules:

- Registry persistence before the first venue call is an internal fatal error. Do not call the venue; mark the system source critical and append a best-effort audit error before returning `Err`.
- Once venue mutation has begun, never use `?` on cancel/list/position/reconcile failures. Append a sanitized message to `venue_errors`, create an account-level unresolved entry, continue any safe remaining step whose deadline has not expired, persist the incomplete response/audit, and return `Ok(response)` with `success=false`.
- When the shared deadline expires, do not start another gateway operation. Record `deadline_exceeded` in `venue_errors`, persist paused/degraded state and the critical audit response, then return incomplete.
- A final trading/registry/status/audit persistence failure returns `Err` after a best-effort critical system alert; it can never become HTTP 200/409 success evidence.

Idempotency rules:

- A repeated call against paused deployments with no venue orders and no unexplained venue positions returns success.
- A repeated venue cancellation response does not fabricate new order transitions.
- A persistence failure never returns success.
- Partial cancel remains paused/degraded and returns `success=false`; the daemon stays available for retry.

Step 1: Add failing tests.

```rust
fn emergency_stop_persists_pause_before_venue_cancel()
fn emergency_stop_stops_workers_before_account_cancel()
fn cancel_all_only_terminalizes_venue_confirmed_orders()
fn emergency_stop_partial_cancel_persists_unresolved_orders()
fn emergency_stop_missing_venue_id_is_incomplete()
fn emergency_stop_cancel_transport_error_still_lists_and_persists_audit()
fn emergency_stop_list_transport_error_returns_structured_incomplete_result()
fn emergency_stop_unknown_or_mismatched_position_is_incomplete()
fn emergency_stop_deadline_persists_incomplete_without_starting_next_call()
fn emergency_stop_empty_live_state_never_calls_gateway()
fn emergency_stop_is_idempotent_after_zero_open_orders()
fn emergency_stop_persistence_failure_does_not_call_venue()
fn emergency_stop_persistence_failure_records_critical_alert_and_audit_error()
fn emergency_stop_writes_structured_audit_receipt()
fn emergency_stop_receipt_carries_stable_nonsecret_daemon_boot_id()
fn emergency_stop_partial_cancel_persists_critical_alert()
```

Use an ordered fake gateway and test-only persistence failpoints already used in `runtime.rs`.

Step 2: Run RED.

```bash
rtk cargo test -p ploy-daemon-host emergency_stop_persists_pause_before_venue_cancel --lib
rtk cargo test -p ploy-daemon-host cancel_all_only_terminalizes_venue_confirmed_orders --lib
rtk cargo test -p ploy-daemon-host emergency_stop_partial_cancel_persists_unresolved_orders --lib
```

Expected RED result: no daemon-owned quiesce operation exists.

Step 3: Implement and share audit I/O.

- Move the existing append/read JSONL helpers from `http.rs` into `audit_io.rs`; do not create a second audit format.
- Keep `EmergencyStopResponse` deterministic by sorting deployment/order ID lists and unresolved-position rows.
- Generate one non-secret UUID `daemon_boot_id` at daemon construction, keep it stable for that process, include it in every emergency response, and persist the full typed response in the canonical/audit adapter. Restart creates a different ID; tests inject fixed IDs.
- Do not hold a separate per-worker lock or add a new service.

Step 4: Regenerate contracts and verify.

```bash
cargo run -p ploy-operator-contracts --example export_schemas
rtk cargo test -p ploy-operator-contracts
rtk cargo test -p ploy-daemon-host emergency_stop --lib
rtk cargo test -p ploy-daemon-host --lib
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-operator-contracts/src/system.rs \
  crates/ploy-operator-contracts/src/lib.rs \
  crates/ploy-operator-contracts/src/schemas.rs \
  contracts/schemas/emergency-stop-response.schema.json \
  crates/ploy-daemon-host/src/audit_io.rs \
  crates/ploy-daemon-host/src/lib.rs \
  crates/ploy-daemon-host/src/http.rs \
  crates/ploy-daemon-host/src/runtime.rs
git commit -m "feat(safety): add canonical emergency quiesce"
```

---

### Task 6: Expose quiesce through Admin API, ployctl, SIGINT, and SIGTERM

Files:

- Modify `crates/ploy-daemon-host/src/http.rs`.
- Modify `crates/ploy-daemon-host/src/runtime.rs`.
- Modify `crates/ploy-control-client/src/lib.rs`.
- Modify `apps/ployctl/src/system.rs`.
- Modify `apps/ployctl/src/main.rs` or its existing command parser module.
- Modify `apps/new-ployd/Cargo.toml`.
- Add `apps/new-ployd/src/lib.rs` for the signal/quiesce coordinator shared by the binary and process fixture.
- Modify `apps/new-ployd/src/main.rs`.
- Add `apps/new-ployd/tests/sigterm.rs`.

Interfaces:

```rust
// Control client
impl ControlPlaneClient {
    pub fn emergency_stop(&self) -> Result<EmergencyStopResponse, String>;
}

// CLI formatter
pub fn emergency_stop(client: &ControlPlaneClient) -> Result<String, String>;
```

CLI command:

```text
ployctl system emergency-stop
```

HTTP contract:

- `POST /api/emergency-stop`
- `required_access()` returns `RequiredAccess::Admin`.
- Success returns HTTP 200 and `success=true`.
- Incomplete quiesce returns HTTP 409 with the full structured response.
- Persistence/internal failure returns HTTP 500; it is not rewritten as success.
- Only after an HTTP 200 response has been fully written and flushed does the handler request clean daemon shutdown.
- Add a status-aware control-client helper that accepts only HTTP 200 or 409 for this route and deserializes `EmergencyStopResponse` for both. `ployctl` prints the full typed body and exits non-zero when `success=false`; it does not discard a 409 body into a generic string error.

Signal contract:

- Add Tokio with workspace features to `new-ployd`; do not add `ctrlc` or `nix`.
- One listener handles Ctrl-C and Unix SIGTERM.
- It locks the same daemon and invokes `emergency_stop_until(Instant::now() + deadline)` on a dedicated blocking task, then awaits that task to completion before process exit. The quiesce itself enforces the absolute deadline cooperatively in every gateway call; no timed-out background task is abandoned while it can still mutate state.
- Default deadline is 15 seconds from `PLOY_EMERGENCY_STOP_DEADLINE_MS`.
- Success exits zero.
- Deadline, lock failure, persistence failure, or unresolved orders exits non-zero after recording a critical audit result.

Replace the boolean shutdown flag with a typed outcome:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    Clean,
    EmergencyIncomplete,
}

pub fn request_shutdown(outcome: ShutdownOutcome);
pub fn run_shared_forever(
    daemon: Arc<Mutex<PloyDaemon>>,
    events: Arc<EventBroker>,
) -> io::Result<ShutdownOutcome>;
```

Step 1: Add failing tests.

```rust
fn emergency_stop_requires_admin()
fn emergency_stop_incomplete_returns_conflict_and_keeps_daemon_running()
fn emergency_stop_http_response_is_flushed_before_shutdown_request()
fn parses_system_emergency_stop_command()
fn control_client_decodes_200_and_409_emergency_stop_response_bodies()
fn cli_returns_nonzero_for_typed_incomplete_response()
```

Process test `apps/new-ployd/tests/sigterm.rs`:

- Start `CARGO_BIN_EXE_new-ployd` with an isolated empty registry/runtime root and `127.0.0.1:0`.
- Wait for `new-ployd booted` on stderr.
- Execute `kill -TERM <pid>` on Unix.
- Poll `try_wait()` with a bounded loop.
- Assert exit code zero, stderr contains `shutdown complete`, and final trading-state/registry snapshots exist.
- Put signal waiting, bounded `spawn_blocking` quiesce, `request_shutdown`, and outcome-to-exit mapping in `apps/new-ployd/src/lib.rs`; `main.rs` and the integration fixture call the same coordinator.
- Add a second parent/child fixture in the integration-test binary: the parent spawns `std::env::current_exe()` filtered to one child test and sets a private fixture-mode environment variable; only that child constructs `PloyDaemon::boot_with_live_execution` with `StaticExecutionGateway`, a valid isolated live registry, and a short deadline. Send SIGTERM to that child and assert non-zero exit, a persisted paused/degraded registry, `deadline_exceeded` in the audit receipt, and no gateway call started after the deadline. Do not add a production environment switch or contact the venue.

Step 2: Run RED.

```bash
rtk cargo test -p ploy-daemon-host emergency_stop_requires_admin --lib
rtk cargo test -p ployctl parses_system_emergency_stop_command
rtk cargo test -p new-ployd --test sigterm -- --nocapture
```

Expected RED result: route/command/signal listener do not exist.

Step 3: Implement the shared call path.

- Do not call `request_shutdown()` from `PloyDaemon::emergency_stop()`; callers decide whether to terminate.
- Keep the API response flush ordering visible in `handle_connection()`.
- Ensure the process test never loads a live key or contacts the venue because its registry is empty.

Step 4: Verify.

```bash
rtk cargo test -p ploy-daemon-host emergency_stop --lib
rtk cargo test -p ploy-control-client emergency_stop --lib
rtk cargo test -p ployctl system
rtk cargo test -p new-ployd --test sigterm -- --nocapture
rtk git diff --check
```

Step 5: Commit.

```bash
git add crates/ploy-daemon-host/src/http.rs \
  crates/ploy-daemon-host/src/runtime.rs \
  crates/ploy-control-client/src/lib.rs \
  apps/ployctl/src/system.rs \
  apps/ployctl/src/main.rs \
  apps/new-ployd/Cargo.toml \
  apps/new-ployd/src/lib.rs \
  apps/new-ployd/src/main.rs \
  apps/new-ployd/tests/sigterm.rs
git commit -m "feat(operations): wire emergency stop to every control path"
```

---

### Task 7: Define the promotion contract and remove the unsafe direct live path

Files:

- Add `scripts/validate_live_promotion_gate.py`.
- Add `tests/test_live_promotion_gate.py`.
- Modify `scripts/drills/pm5d_threelayer_live_gate.sh`.
- Modify `docs/runbooks/live-deployment-checklist.md`.
- Modify `docs/runbooks/rollback.md` only if it describes direct process termination that bypasses quiesce.
- Modify `tasks/todo.md` with exact verification results.

Offline contract and temporary hard stop:

- Remove the current direct `deployments resume` implementation. In this prerequisite slice, `--go-live` always exits non-zero with `canonical_live_promotion_gate_not_installed`; it cannot be re-enabled by an environment variable. V2 Task 6 later adds the canonical daemon operation, Agent Task 2 supplies the retained parity schema, and Packaging Task 4 supplies the protected workflow/provenance check.
- `--skip-dry-run-drill` may remain available only for the default paused-apply diagnostic path and can never accompany `--go-live`.
- The offline validator requires `PLOY_RECORDED_PARITY_JSON`, `PLOY_RECORDED_PARITY_SHA256`, and `PLOY_LIVE_APPROVAL_JSON`. Missing files, hash mismatch, unknown fields, non-finite metrics, or any warning is a hard failure before it emits a non-authorizing candidate receipt.
- Recorded parity must be an immutable successful `main` artifact with `evidence_stage=runtime_parity`, `strict_parity_ready=true`, zero blockers, and exact equality for horizon, symbols, strategy profile, runtime score, model SHA-256, live-config SHA-256, runner git SHA, candidate-replay ID/SHA, recording SHA, executable cost, weighted average entry, maximum drawdown, and account cap. It must refer to the same dry-run candidate that passed executable replay; metadata-only receipts are rejected.
- The approval JSON is a strict, bounded object containing a unique `approval_id`, human `approved_by`, `approved_at`, `expires_at`, exact main SHA, parity artifact SHA-256, candidate replay ID, config/model/runner hashes, normalized account ID, `max_account_exposure_usd=5.0`, and explicit acknowledgement string `APPROVE_PM5D_USD5_FAK_CANARY`. It cannot be model-authored evidence, cannot approve PM15D/PM1H, and expires after at most 24 hours.
- `validate_live_promotion_gate.py` canonicalizes both artifacts, validates every equality, and writes a sanitized `live-promotion-gate.v1` receipt to a caller-supplied mode-0600 path. It prints IDs/hashes only, never account credentials or secrets. The shell script invokes it with `exec`-style argv, never interpolated JSON.
- Define the required future authenticated V2/geoblock probe, positive balance, collateral allowance, canonical PostgreSQL health, zero unresolved/unknown orders and positions, fresh venue heartbeat, and emergency-stop recovery receipt in the validator schema, but do not fake those reads in Bash. V2 Task 6 owns the typed daemon operation that proves them.
- The offline validator may emit a sanitized candidate receipt for tests, but it is not canonical approval, cannot consume an approval ID, and cannot resume a deployment. A local file plus caller-provided SHA is never treated as successful-main provenance.
- The Research Agent and research workflows cannot invoke this script, create approval JSON, or access the trade environment. Later production execution remains a human-triggered protected-environment action from immutable `main`; this local task only builds and tests the gate.

Step 1: Add failing contract tests.

```text
go_live_rejects_missing_or_warning_only_dry_run_drill
go_live_rejects_metadata_only_or_hash_mismatched_parity
go_live_requires_strict_parity_and_exact_candidate_config_model_runner_horizon
go_live_rejects_expired_wrong_wallet_or_over_cap_approval
offline_contract_requires_balance_allowance_geoblock_health_and_recovery_fields
go_live_is_hard_blocked_until_canonical_daemon_gate_exists
offline_validator_accepts_one_exact_human_approved_usd5_pm5d_shape_without_authorizing_live
research_agent_cannot_create_or_execute_live_approval
```

Step 2: Run RED.

```bash
rtk pytest tests/test_live_promotion_gate.py -q
rtk pytest tests/test_strategy_config_contracts.py -q
```

Expected RED result: the current script can warn-and-skip the paper drill and resume without immutable parity, balance/allowance proof, or approval evidence.

Step 3: Implement the validator and fail-closed shell ordering.

- Keep all venue/account reads out of Python/Bash; do not add direct private-key handling or pretend that an offline receipt is canonical.
- Use temporary files with `umask 077` and remove them on every exit path.
- Keep the default invocation paused-only and free of real venue mutations.

Documentation requirements:

- State that `desired=Running, observed=Degraded` is not live-ready.
- Require fresh public + authenticated venue probe, zero unresolved canonical orders, zero unknown external orders/positions, and normalized wallet/cap proof.
- Require the emergency-stop API/CLI/SIGTERM drill before future live approval.
- Document the exact parity and approval schemas, 24-hour expiry, the temporary hard stop, and the downstream V2/Agent/Packaging tasks that install the canonical audit/protected trigger.
- State that the committed live manifest is a paused template and must be rendered with the actual funder/proxy wallet.
- Keep first canary at one strategy, FAK, total account cap USD 5.

Run the full local gate:

```bash
rtk cargo fmt --all -- --check
rtk cargo test --locked \
  -p ploy-connectivity \
  -p ploy-platform \
  -p ploy-platform-runtime \
  -p ploy-daemon-host \
  -p ploy-operator-contracts \
  -p ploy-control-client \
  -p ployctl \
  -p new-ployd
rtk cargo clippy --locked \
  -p ploy-connectivity \
  -p ploy-platform \
  -p ploy-platform-runtime \
  -p ploy-daemon-host \
  -p ploy-operator-contracts \
  -p ploy-control-client \
  -p ployctl \
  -p new-ployd \
  --all-targets -- -D warnings
rtk pytest tests/test_strategy_config_contracts.py
rtk pytest tests/test_live_promotion_gate.py -q
bash -n scripts/drills/pm5d_threelayer_live_gate.sh
python3 -m py_compile scripts/validate_live_promotion_gate.py
rtk git diff --check
```

Expected result: all focused tests pass with no real venue call. If full clippy exposes a documented pre-existing warning outside touched files, record it separately and keep all touched targets warning-free.

Commit the evidence/docs slice:

```bash
git add scripts/validate_live_promotion_gate.py \
  scripts/drills/pm5d_threelayer_live_gate.sh \
  tests/test_live_promotion_gate.py \
  docs/runbooks/live-deployment-checklist.md \
  docs/runbooks/rollback.md \
  tasks/todo.md
git commit -m "fix(safety): disable direct live and define promotion contract"
```

## Completion Criteria

- Risk increase cannot pass without live running/running/fresh state.
- Hedge risk effect is computed from canonical position rather than strategy naming.
- Live wallet/account cap invariants fail closed at apply and restore.
- Initial live entries/hedges use FAK.
- Missing venue IDs and partial cancel remain unresolved.
- Idle live deployments contact both public and authenticated venue surfaces.
- API, CLI, SIGINT, and SIGTERM use one tested quiesce implementation.
- Success is impossible while venue/canonical orders or unexplained positions remain unresolved.
- This slice removes direct live resume entirely; the final repository may re-enable it only through the later canonical V2 operation plus immutable parity/protected-workflow checks.
- All manifests remain paused; no deployment or live call occurred.
