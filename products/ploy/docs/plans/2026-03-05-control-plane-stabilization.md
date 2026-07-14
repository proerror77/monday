# Strategy Control Plane Stabilization Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make strategy deployment/listing behavior consistent by unifying enable-gate semantics and removing unsafe strategy fallback routing.

**Architecture:** Keep current API surface but centralize enable-gate logic into a shared helper used by both deployment endpoints and strategy-control mutation endpoint. In platform bootstrap, replace implicit unknown-strategy fallback with explicit mapping + warning-only behavior.

**Tech Stack:** Rust, Axum handlers, Tokio, SQLx, existing integration/unit tests.

---

### Task 1: Shared Deployment Enable Gate

**Files:**
- Create: `src/api/handlers/deployment_gate.rs`
- Modify: `src/api/handlers/mod.rs`
- Modify: `src/api/handlers/deployments.rs`
- Modify: `src/api/handlers/strategies.rs`

**Steps:**
1. Move/implement evidence-gate validation in a shared helper module.
2. Keep request/response schema unchanged.
3. Make `/api/deployments` enable path call the shared helper.
4. Make `/api/strategies/control/:id` enabling path call the same helper before writing `enabled=true`.
5. Ensure disabling remains unaffected.

### Task 2: Explicit Strategy Mapping in Bootstrap

**Files:**
- Modify: `src/coordinator/bootstrap.rs`

**Steps:**
1. Introduce explicit strategy-kind resolver for crypto strategy keys.
2. Remove implicit unknown-strategy => momentum fallback.
3. Log warning for unknown strategy keys with deployment id/strategy name.
4. Keep existing known aliases (`momentum`, `split_arb`, `pattern_memory`, `lob_ml`).
5. Preserve existing behavior for known strategies.

### Task 3: Tests and Gate Semantics Drift Cleanup

**Files:**
- Modify: `src/coordinator/bootstrap.rs` tests section
- Modify: `tests/legacy_live_gate.rs`

**Steps:**
1. Add/adjust bootstrap tests to assert unknown crypto strategy does not auto-enable momentum.
2. Update legacy live-gate test expectations to match documented behavior: blocked by default, explicit env override allowed.
3. Run targeted tests:
   - `cargo test --test legacy_live_gate`
   - `cargo test apply_strategy_deployments --lib`

### Task 4: Validation and Atomic Commits

**Files:**
- Modify: `tasks/todo.md`

**Steps:**
1. Mark completed checklist items.
2. Stage explicit paths only.
3. Review staged diff.
4. Create atomic commits:
   - `api: unify deployment enable evidence gate`
   - `coordinator: remove unknown strategy fallback routing`
   - `tests: align legacy live gate expectations`
