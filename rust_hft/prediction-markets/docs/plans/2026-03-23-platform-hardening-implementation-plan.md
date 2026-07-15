# Platform Hardening Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add internal metrics/alerts, stale-source degradation, and finer operator permissions to the workspace control-plane runtime.

**Architecture:** Extend the existing `ployd` daemon state with a small health/heartbeat projection, expose it through new operator contracts and endpoints, then wire the same semantics through CLI/TUI/frontend while refining auth access classes from coarse token roles into capability bands.

**Tech Stack:** Rust, Axum-style HTTP helpers, SSE event streaming, serde contracts, React frontend, focused `rtk cargo test` and `npm run build/lint`

---

### Task 1: Track the hardening lane

**Files:**
- Modify: `tasks/todo.md`
- Reference: `docs/plans/2026-03-23-platform-hardening-design.md`

**Step 1: Add a new top-of-file tracker section**

Record:
- metrics/alerts
- stale-source degradation
- auth scope refinement

**Step 2: Record file ownership**

Call out:
- `apps/ployd/src/runtime.rs`, `apps/ployd/src/http.rs`, `apps/ployd/src/config.rs`
- `crates/ploy-platform/src/system.rs`
- `crates/ploy-operator-contracts/src/{system,events}.rs`
- `apps/ployctl/src/system.rs`
- `apps/ploytui/src/lib.rs`
- `ploy-frontend/src/{services/websocket.ts,pages/SystemControl.tsx,types/index.ts}`

**Step 3: Commit the planning docs**

```bash
git add tasks/todo.md docs/plans/2026-03-23-platform-hardening-design.md docs/plans/2026-03-23-platform-hardening-implementation-plan.md
git commit -m "docs: plan platform hardening work"
```

### Task 2: Add contracts and daemon health state

**Files:**
- Modify: `crates/ploy-operator-contracts/src/system.rs`
- Modify: `crates/ploy-operator-contracts/src/events.rs`
- Modify: `crates/ploy-operator-contracts/src/lib.rs`
- Modify: `crates/ploy-platform/src/system.rs`
- Modify: `apps/ployd/src/runtime.rs`
- Modify: `apps/ployd/src/config.rs`

**Step 1: Add failing contract/unit coverage**

Write tests for:
- metrics serialization
- alert serialization
- stale-source projection into system state

**Step 2: Run the focused tests and confirm they fail**

```bash
rtk cargo test -p ploy-operator-contracts system -- --nocapture
rtk cargo test -p ploy-platform system -- --nocapture
```

**Step 3: Add contract types**

Add:
- `PlatformMetrics`
- `ActiveAlert`
- `AlertSeverity`
- `AlertKind`
- `HeartbeatStatus`
- `MetricsSnapshotEvent`
- `AlertSnapshotEvent`

**Step 4: Extend daemon/system state**

Add source heartbeat tracking for:
- deployment workers
- live reconcile
- venue connectivity
- claim loop

Add config thresholds for staleness windows.

**Step 5: Project degraded/recovering from stale sources**

Update the daemon tick path so stale critical sources raise alerts and mark the platform degraded; recovery should transition back through `recovering`.

**Step 6: Run focused verification**

```bash
rtk cargo test -p ploy-operator-contracts -p ploy-platform -p ployd
```

**Step 7: Commit**

```bash
git add crates/ploy-operator-contracts crates/ploy-platform apps/ployd/src/runtime.rs apps/ployd/src/config.rs
git commit -m "runtime: add platform metrics and stale-source health"
```

### Task 3: Expose metrics and alerts through operator surfaces

**Files:**
- Modify: `apps/ployd/src/http.rs`
- Modify: `apps/ployctl/src/client.rs`
- Modify: `apps/ployctl/src/system.rs`
- Modify: `apps/ployctl/src/main.rs`
- Modify: `apps/ploytui/src/lib.rs`
- Modify: `ploy-frontend/src/services/websocket.ts`
- Modify: `ploy-frontend/src/pages/SystemControl.tsx`
- Modify: `ploy-frontend/src/types/index.ts`

**Step 1: Add failing endpoint/client tests**

Cover:
- `/api/system/metrics`
- `/api/system/alerts`
- SSE event serialization for metrics/alerts
- CLI rendering for the new snapshots

**Step 2: Run the focused tests and confirm they fail**

```bash
rtk cargo test -p ployd http -- --nocapture
rtk cargo test -p ployctl system -- --nocapture
```

**Step 3: Implement HTTP and SSE surfaces**

Expose:
- `GET /api/system/metrics`
- `GET /api/system/alerts`
- `metrics_snapshot`
- `alert_snapshot`

**Step 4: Wire CLI/TUI/frontend**

- `ployctl system metrics`
- `ployctl system alerts`
- TUI system block shows alert count/stale-source summary
- frontend renders metrics/alerts and handles new SSE event kinds

**Step 5: Run verification**

```bash
rtk cargo test -p ployd -p ployctl -p ploytui
cd ploy-frontend && npm run build && npm run lint
```

**Step 6: Commit**

```bash
git add apps/ployd/src/http.rs apps/ployctl apps/ploytui ploy-frontend
git commit -m "operator: expose platform metrics and alerts"
```

### Task 4: Refine auth scopes and document the result

**Files:**
- Modify: `apps/ployd/src/http.rs`
- Modify: `apps/ployctl/src/client.rs` if needed
- Modify: `docs/runbooks/platform-startup.md`
- Modify: `docs/runbooks/platform-deploy.md`
- Modify: `README.md`

**Step 1: Add failing auth tests**

Cover:
- read endpoints accepted for sidecar/admin
- operator/admin split for write endpoints
- browser session still works for SSE

**Step 2: Run the focused auth tests**

```bash
rtk cargo test -p ployd auth -- --nocapture
```

**Step 3: Refine required access**

Replace the coarse `ReadOnly/Admin` split with:
- `Public`
- `Read`
- `Operator`
- `Admin`

Keep existing tokens/cookies; only change capability mapping.

**Step 4: Update operator docs**

Document new env vars, stale thresholds, alert endpoints, and access classes.

**Step 5: Run final verification**

```bash
rtk cargo test -p ployd -p ployctl -p ploytui
rtk cargo test --test platform_smoke --test platform_release_workflow
cd ploy-frontend && npm run build && npm run lint
```

**Step 6: Commit**

```bash
git add apps/ployd/src/http.rs apps/ployctl/src/client.rs README.md docs/runbooks/platform-startup.md docs/runbooks/platform-deploy.md
git commit -m "auth: refine operator access scopes"
```
