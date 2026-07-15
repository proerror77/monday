# Operator Terminal V1 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add an API-first operator terminal v1 to `ploy` with a terminal frontend and a unified control-plane contract for global/domain pause, resume, force-close, and claimer actions.

**Architecture:** Reuse the existing Axum API, coordinator control commands, claimer capability, and ratatui dashboard. Add one operator read model endpoint plus one operator action endpoint, then rewire the TUI into a thin client that reads snapshots and dispatches confirmed actions through the API instead of mutating local state.

**Tech Stack:** Rust, Axum, Tokio, Serde, existing `AppState`, existing `CoordinatorControlCommand`, existing claimer logic, existing ratatui TUI, targeted unit tests and CLI/TUI smoke checks, `rtk` wrappers for validation.

---

### Task 1: Add Operator API Types And Tests

**Files:**
- Modify: `src/api/types.rs`
- Test: `src/api/types.rs`

**Step 1: Write the failing tests**

Add tests for:

- operator action request serialization
- operator action response serialization
- operator status response serialization
- domain action scope validation

Use a skeleton like:

```rust
#[test]
fn operator_action_request_serializes_pause_domain() {
    let req = OperatorActionRequest {
        action: OperatorAction::Pause,
        scope: OperatorScope::Domain,
        domain: Some("crypto".to_string()),
        requested_by: "test".to_string(),
        reason: Some("ops".to_string()),
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"pause\""));
    assert!(json.contains("\"domain\""));
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
rtk cargo test operator_action_request_serializes_pause_domain --lib -- --exact --nocapture
```

Expected: FAIL because the operator API types do not exist yet.

**Step 3: Add the minimal API contract**

Define:

- `OperatorAction`
- `OperatorScope`
- `OperatorActionRequest`
- `OperatorActionResponse`
- `OperatorStatusResponse`
- `OperatorDomainStatus`
- `OperatorClaimerStatus`
- `OperatorRecentAction`

Keep the surface narrow and v1-only. Do not add deployment-level action types.

**Step 4: Run the tests and a compile check**

Run:

```bash
rtk cargo test operator_action_request_serializes_pause_domain --lib -- --exact --nocapture
rtk cargo check --features api --bin ploy
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/api/types.rs
git commit -m "api: add operator terminal contract types"
```

### Task 2: Add Operator Read Model Endpoint

**Files:**
- Create: `src/api/handlers/operator.rs`
- Modify: `src/api/handlers/mod.rs`
- Modify: `src/api/routes.rs`
- Modify: `src/api/state.rs`
- Test: `src/api/handlers/operator.rs`

**Step 1: Write the failing handler tests**

Add tests for:

- `GET /api/operator/status` returns system and governance fields
- missing coordinator still returns a degraded but valid response
- claimer status fields are present even when capability is disabled

Use a skeleton like:

```rust
#[tokio::test]
async fn operator_status_returns_runtime_snapshot() {
    let state = test_app_state();
    let response = get_operator_status(State(state)).await.into_response();
    assert_eq!(response.status(), StatusCode::OK);
}
```

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test operator_status_returns_runtime_snapshot --lib -- --exact --nocapture
```

Expected: FAIL because the operator handler does not exist yet.

**Step 3: Implement the read model**

Add a handler that assembles:

- system status
- governance status
- risk state
- queue depth
- domain ingress summaries
- claimer summary
- recent operator action history

Prefer a small service helper inside `operator.rs` or `api/state.rs`. Do not spread this aggregation across the TUI.

**Step 4: Register the route**

Add:

```text
GET /api/operator/status
```

Require the same admin auth posture as the rest of the control plane.

**Step 5: Run tests and a compile check**

Run:

```bash
rtk cargo test operator_status_returns_runtime_snapshot --lib -- --exact --nocapture
rtk cargo check --features api --bin ploy
```

Expected: PASS

**Step 6: Commit**

```bash
git add src/api/handlers/operator.rs src/api/handlers/mod.rs src/api/routes.rs src/api/state.rs
git commit -m "api: add operator status endpoint"
```

### Task 3: Add Unified Operator Action Dispatch

**Files:**
- Create: `src/api/handlers/operator.rs`
- Modify: `src/api/state.rs`
- Modify: `src/coordinator/command.rs`
- Test: `src/api/handlers/operator.rs`

**Step 1: Write the failing action tests**

Add tests for:

- pause-all action maps to `CoordinatorControlCommand::PauseAll`
- pause-domain action rejects missing domain
- claim-check action returns `not_supported` when claimer is unavailable
- force-close-domain action writes an action receipt

Use a skeleton like:

```rust
#[tokio::test]
async fn pause_all_operator_action_returns_accepted_receipt() {
    let state = test_app_state_with_coordinator();
    let req = sample_operator_request(OperatorAction::Pause, OperatorScope::Global);
    let response = post_operator_action(State(state), Json(req)).await.into_response();
    assert_eq!(response.status(), StatusCode::OK);
}
```

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test pause_all_operator_action_returns_accepted_receipt --lib -- --exact --nocapture
```

Expected: FAIL because the action endpoint and dispatch path do not exist yet.

**Step 3: Implement the action endpoint**

Add:

```text
POST /api/operator/actions
```

Support exactly:

- `pause_all`
- `resume_all`
- `force_close_all`
- `pause_domain`
- `resume_domain`
- `force_close_domain`
- `claim_check`
- `claim_run`

Return a unified `OperatorActionResponse`.

**Step 4: Add dispatch helpers**

Map operator actions to:

- `CoordinatorControlCommand::*`
- claimer check helper
- one-shot claimer run helper

If claimer capability is not available, return an explicit non-500 response payload. Do not silently succeed.

**Step 5: Add lightweight audit state**

Store a small in-memory recent action ring in `AppState` for v1. Persisted audit can come later.

**Step 6: Run tests and compile**

Run:

```bash
rtk cargo test pause_all_operator_action_returns_accepted_receipt --lib -- --exact --nocapture
rtk cargo check --features api --bin ploy
```

Expected: PASS

**Step 7: Commit**

```bash
git add src/api/handlers/operator.rs src/api/state.rs src/coordinator/command.rs
git commit -m "api: add unified operator action dispatch"
```

### Task 4: Wire Claimer Capability Into Operator Control Plane

**Files:**
- Modify: `src/main_modes/claimer_mode.rs`
- Modify: `src/api/state.rs`
- Modify: `src/api/handlers/operator.rs`
- Test: `src/main_modes/claimer_mode.rs`
- Test: `src/api/handlers/operator.rs`

**Step 1: Write the failing tests**

Add tests for:

- one-shot claimer helper can produce a summary without exiting the process
- operator claim-check uses helper in read-only mode
- operator claim-run uses helper in action mode

Use a skeleton like:

```rust
#[tokio::test]
async fn claimer_check_helper_returns_structured_summary() {
    let summary = run_claimer_once_for_operator(test_client(), true).await.unwrap();
    assert!(summary.checked_at.is_some());
}
```

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test claimer_check_helper_returns_structured_summary --lib -- --exact --nocapture
```

Expected: FAIL because the claimer helper is still CLI-oriented.

**Step 3: Extract a reusable one-shot helper**

Refactor the claimer entrypoint so the API can call a structured one-shot helper without:

- printing CLI output
- exiting the process
- duplicating redeem logic

Keep the current CLI command behavior intact by delegating to the new helper.

**Step 4: Run focused tests and compile**

Run:

```bash
rtk cargo test claimer_check_helper_returns_structured_summary --lib -- --exact --nocapture
rtk cargo check --features api --bin ploy
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/main_modes/claimer_mode.rs src/api/state.rs src/api/handlers/operator.rs
git commit -m "claimer: expose one-shot operator control helper"
```

### Task 5: Extend TUI State For Operator Mode

**Files:**
- Modify: `src/tui/data.rs`
- Modify: `src/tui/app.rs`
- Modify: `src/tui/event.rs`
- Test: `src/tui/app.rs`

**Step 1: Write the failing TUI state tests**

Add tests for:

- operator snapshot updates app state
- selected domain changes correctly
- pending operator action modal carries scope and action

Use a skeleton like:

```rust
#[test]
fn operator_snapshot_updates_selected_domain_list() {
    let mut app = TuiApp::new();
    app.update_operator_status(sample_operator_status());
    assert_eq!(app.operator_domains.len(), 2);
}
```

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test operator_snapshot_updates_selected_domain_list --lib -- --exact --nocapture
```

Expected: FAIL because operator TUI state does not exist yet.

**Step 3: Add operator-focused app state**

Extend the app with:

- operator summary snapshot
- selected domain index
- recent actions
- claimer panel data
- pending operator action request

Add key actions for:

- select domain
- pause/resume/force-close selected scope
- claim-check
- claim-run
- explicit refresh

**Step 4: Run tests and compile**

Run:

```bash
rtk cargo test operator_snapshot_updates_selected_domain_list --lib -- --exact --nocapture
rtk cargo check --bin ploy
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/tui/data.rs src/tui/app.rs src/tui/event.rs
git commit -m "tui: add operator terminal state model"
```

### Task 6: Rewire TUI Runner To Use Operator API

**Files:**
- Modify: `src/tui/runner.rs`
- Modify: `src/tui/ui.rs`
- Modify: `src/tui/widgets/mod.rs`
- Create: `src/tui/widgets/operator.rs`
- Test: `src/tui/runner.rs`

**Step 1: Write the failing runner tests**

Add tests for:

- TUI refresh fetches `/api/operator/status`
- confirmed action posts `/api/operator/actions`
- local keypress no longer mutates fake running state directly

Use a skeleton like:

```rust
#[tokio::test]
async fn operator_refresh_pulls_status_from_api() {
    let server = spawn_test_operator_api();
    let mut runner = test_runner_against(server.url());
    let result = runner.refresh_operator_status().await;
    assert!(result.is_ok());
}
```

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test operator_refresh_pulls_status_from_api --lib -- --exact --nocapture
```

Expected: FAIL because the runner does not call the operator API yet.

**Step 3: Add a small operator API client inside the TUI layer**

Keep it minimal:

- fetch status
- submit action

Do not add a generic SDK in v1 unless it is already clearly present elsewhere.

**Step 4: Replace local fake control mutations**

When the user confirms pause/resume/force-close/claim:

- call the API
- refresh status
- append action result to recent actions

Remove the current placeholder behavior where modals only mutate local `strategy_state`.

**Step 5: Add operator panels**

Render:

- operator summary
- domain control
- claimer
- recent actions

Reuse existing visual language and keep the current portfolio/agent-monitor views intact.

**Step 6: Run tests and compile**

Run:

```bash
rtk cargo test operator_refresh_pulls_status_from_api --lib -- --exact --nocapture
rtk cargo check --bin ploy
```

Expected: PASS

**Step 7: Commit**

```bash
git add src/tui/runner.rs src/tui/ui.rs src/tui/widgets/mod.rs src/tui/widgets/operator.rs
git commit -m "tui: wire operator terminal through api control plane"
```

### Task 7: Add Docs And Operator Smoke Coverage

**Files:**
- Modify: `README.md`
- Create: `docs/runbooks/operator-terminal.md`
- Test: `src/api/handlers/operator.rs`
- Test: `src/tui/runner.rs`

**Step 1: Write the failing smoke tests**

Add or extend smoke coverage for:

- operator status endpoint returns 200
- operator action endpoint returns accepted receipt for pause-all
- TUI operator refresh can parse the response

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test operator_status_returns_runtime_snapshot --lib -- --nocapture
rtk cargo test operator_refresh_pulls_status_from_api --lib -- --nocapture
```

Expected: FAIL before the docs-aligned smoke path is complete.

**Step 3: Update docs**

Document:

- operator terminal purpose
- supported v1 actions
- safety constraints
- example operator flow

Add a small runbook like:

```bash
ploy serve --port 8081
ploy dashboard
```

Inside the TUI:

- refresh operator snapshot
- pause domain
- resume domain
- run claim check

**Step 4: Run final validation**

Run:

```bash
rtk cargo check --features api --bin ploy
rtk cargo test operator_status_returns_runtime_snapshot --lib -- --nocapture
rtk cargo test pause_all_operator_action_returns_accepted_receipt --lib -- --nocapture
rtk cargo test operator_refresh_pulls_status_from_api --lib -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add README.md docs/runbooks/operator-terminal.md src/api/handlers/operator.rs src/tui/runner.rs
git commit -m "docs: add operator terminal runbook and smoke coverage"
```
