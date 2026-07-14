# Trading Platform Refactor Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Reshape `ploy` into an API-first trading platform with a single `ployd` control plane, deployment workers for strategy bundles, a canonical trading lifecycle, and a real multi-crate Rust workspace.

**Architecture:** Create a small workspace with `ployd` and `ployctl` app crates plus focused library crates for platform, trading, connectivity, deployments, operator contracts, strategy bundles, and research. Introduce the new platform spine first, add compatibility shims around existing runtime code, then progressively retire legacy bootstrap, strategy, and operator paths.

**Tech Stack:** Rust workspace, Tokio, Axum, SQLx/Postgres, Serde/TOML, existing coordinator and adapter code reused behind new crate boundaries, targeted unit/integration tests, `rtk` command wrappers, frequent atomic commits.

---

### Task 1: Convert The Root Package Into A Real Workspace

**Files:**
- Modify: `Cargo.toml`
- Create: `apps/ployd/Cargo.toml`
- Create: `apps/ployd/src/main.rs`
- Create: `apps/ployctl/Cargo.toml`
- Create: `apps/ployctl/src/main.rs`
- Create: `crates/ploy-platform/Cargo.toml`
- Create: `crates/ploy-platform/src/lib.rs`
- Create: `crates/ploy-trading/Cargo.toml`
- Create: `crates/ploy-trading/src/lib.rs`
- Create: `crates/ploy-connectivity/Cargo.toml`
- Create: `crates/ploy-connectivity/src/lib.rs`
- Create: `crates/ploy-deployments/Cargo.toml`
- Create: `crates/ploy-deployments/src/lib.rs`
- Create: `crates/ploy-operator-contracts/Cargo.toml`
- Create: `crates/ploy-operator-contracts/src/lib.rs`
- Create: `crates/ploy-strategy-bundles/Cargo.toml`
- Create: `crates/ploy-strategy-bundles/src/lib.rs`
- Create: `crates/ploy-research/Cargo.toml`
- Create: `crates/ploy-research/src/lib.rs`
- Modify: `src/main.rs`
- Modify: `src/lib.rs`
- Test: `Cargo.toml`

**Step 1: Write the failing workspace smoke checks**

Define the initial target commands:

```bash
rtk cargo check -p ployd
rtk cargo check -p ployctl
```

Expected: FAIL because the app and library crates do not exist yet.

**Step 2: Add the workspace members and shared dependency table**

Update the root manifest so it owns only:

- workspace members
- shared dependency versions
- shared profiles
- temporary compatibility package metadata if needed

Keep the workspace small. Do not introduce extra crates beyond the approved target shape.

**Step 3: Add minimal app and library crate skeletons**

Each new crate should compile with a minimal `lib.rs` or `main.rs`. Use small placeholders such as:

```rust
pub fn crate_marker() -> &'static str {
    "ploy-platform"
}
```

and:

```rust
fn main() {
    eprintln!("ployd bootstrap not implemented yet");
}
```

**Step 4: Preserve a compatibility path for the current root crate**

If the existing root package must remain temporarily for migration, keep it as a thin compatibility shim only. Do not let it stay the long-term runtime center.

**Step 5: Run workspace checks**

Run:

```bash
rtk cargo check -p ployd
rtk cargo check -p ployctl
rtk cargo check -p ploy-platform
```

Expected: PASS

**Step 6: Commit**

```bash
git add Cargo.toml apps crates src/main.rs src/lib.rs
git commit -m "workspace: add platform workspace skeleton"
```

### Task 2: Introduce Shared Operator Contracts

**Files:**
- Create: `crates/ploy-operator-contracts/src/system.rs`
- Create: `crates/ploy-operator-contracts/src/deployments.rs`
- Create: `crates/ploy-operator-contracts/src/trading.rs`
- Create: `crates/ploy-operator-contracts/src/events.rs`
- Modify: `crates/ploy-operator-contracts/src/lib.rs`
- Modify: `src/api/types.rs`
- Modify: `ploy-frontend/src/types/index.ts`
- Modify: `ploy-sidecar/src/tools/ploy-backend.ts`
- Test: `crates/ploy-operator-contracts/src/deployments.rs`
- Test: `crates/ploy-operator-contracts/src/events.rs`

**Step 1: Write failing contract tests**

Add tests covering:

- deployment status serialization
- websocket event kind serialization
- trading lifecycle DTO stability

Use a skeleton like:

```rust
#[test]
fn observed_state_serializes_as_running() {
    let json = serde_json::to_string(&ObservedState::Running).unwrap();
    assert_eq!(json, "\"running\"");
}
```

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test -p ploy-operator-contracts observed_state_serializes_as_running -- --nocapture
```

Expected: FAIL because the contract crate is still empty.

**Step 3: Add the canonical API and event types**

Define neutral shared types:

```rust
pub enum DesiredState { Running, Paused, Stopped }
pub enum ObservedState { Starting, Running, Degraded, Paused, Stopped, Failed }

pub struct DeploymentSummary {
    pub deployment_id: String,
    pub bundle_id: String,
    pub runtime_mode: RuntimeMode,
    pub desired_state: DesiredState,
    pub observed_state: ObservedState,
}
```

Also add shared trading DTOs and event envelopes. Keep the surface minimal and read/write oriented for platform operations only.

**Step 4: Re-export the shared types and point legacy API types at them**

Start by re-exporting new DTOs from the current API layer rather than deleting old types immediately.

**Step 5: Run the tests and one compile check**

Run:

```bash
rtk cargo test -p ploy-operator-contracts -- --nocapture
rtk cargo check --features api --bin ploy
```

Expected: PASS

**Step 6: Commit**

```bash
git add crates/ploy-operator-contracts src/api/types.rs ploy-frontend/src/types/index.ts ploy-sidecar/src/tools/ploy-backend.ts
git commit -m "contracts: add shared operator surface types"
```

### Task 3: Extract The Canonical Trading Lifecycle

**Files:**
- Create: `crates/ploy-trading/src/intents.rs`
- Create: `crates/ploy-trading/src/orders.rs`
- Create: `crates/ploy-trading/src/fills.rs`
- Create: `crates/ploy-trading/src/positions.rs`
- Create: `crates/ploy-trading/src/pnl.rs`
- Create: `crates/ploy-trading/src/risk.rs`
- Modify: `crates/ploy-trading/src/lib.rs`
- Modify: `src/strategy/position_manager.rs`
- Modify: `src/services/order_monitor.rs`
- Modify: `src/execution/mod.rs`
- Test: `crates/ploy-trading/src/positions.rs`
- Test: `crates/ploy-trading/src/risk.rs`

**Step 1: Write failing lifecycle tests**

Cover:

- fill updates create position state
- cancel does not mutate position state
- risk snapshot reflects active orders and current exposure

Use a skeleton like:

```rust
#[test]
fn fill_updates_position_quantity() {
    let mut ledger = PositionLedger::default();
    ledger.apply_fill(sample_fill("yes", dec!(5)));
    assert_eq!(ledger.net_qty("yes"), dec!(5));
}
```

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test -p ploy-trading fill_updates_position_quantity -- --nocapture
```

Expected: FAIL because the trading lifecycle crate does not exist yet.

**Step 3: Add the lifecycle types and ledgers**

Introduce neutral core types:

```rust
pub struct TradingIntent { /* deployment_id, market, side, quantity, constraints */ }
pub struct OrderRecord { /* intent_id, venue_order_id, state */ }
pub struct FillRecord { /* order_id, qty, price, fee */ }
```

Implement:

- order ledger
- fill ledger
- position ledger
- pnl snapshot
- risk snapshot

Keep the first slice in-memory and deterministic. Do not pull in SQL persistence yet.

**Step 4: Point legacy modules at the new lifecycle types**

Make `position_manager` and `order_monitor` delegate to the new crate instead of defining parallel truth models.

**Step 5: Run tests and compile current runtime**

Run:

```bash
rtk cargo test -p ploy-trading -- --nocapture
rtk cargo check --bin ploy
```

Expected: PASS

**Step 6: Commit**

```bash
git add crates/ploy-trading src/strategy/position_manager.rs src/services/order_monitor.rs src/execution/mod.rs
git commit -m "trading: add canonical lifecycle ledgers"
```

### Task 4: Build The Platform Control Plane Crate

**Files:**
- Create: `crates/ploy-platform/src/control_plane.rs`
- Create: `crates/ploy-platform/src/deployments.rs`
- Create: `crates/ploy-platform/src/system.rs`
- Create: `crates/ploy-platform/src/accounts.rs`
- Create: `crates/ploy-platform/src/audit.rs`
- Create: `crates/ploy-platform/src/health.rs`
- Modify: `crates/ploy-platform/src/lib.rs`
- Modify: `src/api/state.rs`
- Modify: `src/api/routes.rs`
- Modify: `src/api/handlers/system.rs`
- Test: `crates/ploy-platform/src/deployments.rs`
- Test: `src/api/handlers/system.rs`

**Step 1: Write failing deployment registry tests**

Cover:

- create deployment
- update desired state
- derive observed state
- reject duplicate deployment IDs

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test -p ploy-platform create_deployment -- --nocapture
```

Expected: FAIL because the platform crate is still a stub.

**Step 3: Add the control-plane services**

Start with small services:

```rust
pub struct DeploymentRegistry { /* in-memory for first slice */ }
pub struct SystemService { /* start, pause, halt */ }
pub struct AuditLog { /* append-only events */ }
```

Each service should speak only in `ploy-operator-contracts` and `ploy-trading` types.

**Step 4: Route the current API state through the new crate**

Use the new platform services as the single home for:

- deployment state
- system lifecycle state
- audit events
- health aggregation

Avoid re-implementing these concepts inside `src/api`.

**Step 5: Run tests**

Run:

```bash
rtk cargo test -p ploy-platform -- --nocapture
rtk cargo test api::handlers::system --lib -- --nocapture
```

Expected: PASS

**Step 6: Commit**

```bash
git add crates/ploy-platform src/api/state.rs src/api/routes.rs src/api/handlers/system.rs
git commit -m "platform: add control plane services and registry"
```

### Task 5: Add Deployment Worker Protocol And Supervisor

**Files:**
- Create: `crates/ploy-deployments/src/protocol.rs`
- Create: `crates/ploy-deployments/src/supervisor.rs`
- Create: `crates/ploy-deployments/src/runtime.rs`
- Create: `crates/ploy-deployments/src/health.rs`
- Modify: `crates/ploy-deployments/src/lib.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/coordinator/strategy_runtime.rs`
- Test: `crates/ploy-deployments/src/supervisor.rs`
- Test: `src/coordinator/strategy_runtime.rs`

**Step 1: Write failing supervisor tests**

Cover:

- start one deployment worker
- restart a failed worker
- keep deployment desired state separate from worker pid state

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test -p ploy-deployments start_one_worker -- --nocapture
```

Expected: FAIL because the deployment crate is still empty.

**Step 3: Add the worker protocol and supervisor**

Define a minimal protocol:

```rust
pub struct WorkerLaunchSpec {
    pub deployment_id: String,
    pub bundle_id: String,
    pub runtime_mode: RuntimeMode,
}
```

Implement:

- spawn
- heartbeat
- shutdown
- restart-on-failure

Start with local process management only. Do not design for multi-node scheduling in this phase.

**Step 4: Shrink legacy bootstrap into an adapter**

Move orchestration ownership into the new supervisor. Keep `bootstrap` only as a compatibility entry point until the app crates finish migrating.

**Step 5: Run tests**

Run:

```bash
rtk cargo test -p ploy-deployments -- --nocapture
rtk cargo test coordinator::strategy_runtime --lib -- --nocapture
```

Expected: PASS

**Step 6: Commit**

```bash
git add crates/ploy-deployments src/coordinator/bootstrap.rs src/coordinator/strategy_runtime.rs
git commit -m "deployments: add worker protocol and supervisor"
```

### Task 6: Build `ployd` As The Canonical Runtime Entry Point

**Files:**
- Modify: `apps/ployd/src/main.rs`
- Create: `apps/ployd/src/config.rs`
- Create: `apps/ployd/src/http.rs`
- Create: `apps/ployd/src/runtime.rs`
- Modify: `src/main_dispatch.rs`
- Modify: `src/main_modes/platform_mode.rs`
- Modify: `src/lib.rs`
- Test: `apps/ployd/src/runtime.rs`

**Step 1: Write failing daemon startup tests**

Cover:

- `ployd` loads platform config
- `ployd` boots the control plane and supervisor
- `ployd` restores deployment desired state on startup

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test -p ployd daemon_loads_platform_config -- --nocapture
```

Expected: FAIL because `ployd` still has placeholder bootstrap code.

**Step 3: Implement the daemon composition root**

Wire together:

- config loader
- Postgres state
- control-plane services
- worker supervisor
- Axum API / websocket server

`ployd` should become the long-term home for runtime assembly. Do not let legacy `main_dispatch` remain the true composition root.

**Step 4: Keep a compatibility wrapper for the old `ploy` binary**

If the current `ploy` binary must stay temporarily, make it shell out to or embed the same `ployd` runtime path instead of keeping a second path alive.

**Step 5: Run tests and checks**

Run:

```bash
rtk cargo test -p ployd -- --nocapture
rtk cargo check -p ployd
```

Expected: PASS

**Step 6: Commit**

```bash
git add apps/ployd src/main_dispatch.rs src/main_modes/platform_mode.rs src/lib.rs
git commit -m "apps: build ployd daemon entry point"
```

### Task 7: Build `ployctl` And Demote Legacy CLI Logic

**Files:**
- Modify: `apps/ployctl/src/main.rs`
- Create: `apps/ployctl/src/client.rs`
- Create: `apps/ployctl/src/deployments.rs`
- Create: `apps/ployctl/src/system.rs`
- Modify: `src/cli/runtime.rs`
- Modify: `src/cli/strategy.rs`
- Modify: `src/main.rs`
- Test: `apps/ployctl/src/deployments.rs`

**Step 1: Write failing CLI client tests**

Cover:

- list deployments
- apply deployment manifest
- pause deployment
- inspect deployment

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test -p ployctl list_deployments -- --nocapture
```

Expected: FAIL because `ployctl` still has placeholder code.

**Step 3: Implement the API client and commands**

Support the minimum operator contract:

- `system start`
- `deployments apply`
- `deployments list`
- `deployments pause`
- `deployments inspect`

Legacy CLI modules should delegate to the new client or become compatibility shims. Do not preserve direct runtime branching inside CLI command handlers.

**Step 4: Run tests and a compile check**

Run:

```bash
rtk cargo test -p ployctl -- --nocapture
rtk cargo check -p ployctl
```

Expected: PASS

**Step 5: Commit**

```bash
git add apps/ployctl src/cli/runtime.rs src/cli/strategy.rs src/main.rs
git commit -m "cli: add ployctl and demote legacy runtime commands"
```

### Task 8: Move Strategies Into Bundle Runtime And Cut Research Dependency

**Files:**
- Create: `crates/ploy-strategy-bundles/src/bundle.rs`
- Create: `crates/ploy-strategy-bundles/src/runtime.rs`
- Create: `crates/ploy-strategy-bundles/src/signals.rs`
- Modify: `crates/ploy-strategy-bundles/src/lib.rs`
- Create: `crates/ploy-research/src/backtesting.rs`
- Create: `crates/ploy-research/src/replay.rs`
- Modify: `crates/ploy-research/src/lib.rs`
- Modify: `src/strategy/mod.rs`
- Modify: `src/cli/strategy/backtest_ops.rs`
- Modify: `src/strategy/runtime_specs/runtime_plans.rs`
- Test: `crates/ploy-strategy-bundles/src/runtime.rs`
- Test: `crates/ploy-research/src/backtesting.rs`

**Step 1: Write failing bundle runtime tests**

Cover:

- one bundle can emit multiple intents
- bundle runtime has no direct dependency on backtesting code
- research crate can consume trading models without platform depending on research

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test -p ploy-strategy-bundles bundle_emits_multiple_intents -- --nocapture
```

Expected: FAIL because the bundle and research crates are still empty.

**Step 3: Add the strategy bundle runtime**

Move strategy-facing runtime concepts into the bundle crate and keep the contract narrow:

- market inputs in
- intents out

Do not move order truth or risk truth into this crate.

**Step 4: Move research-only code behind a separate crate**

Backtesting and replay code should depend on shared trading types but not on the live platform services.

**Step 5: Run tests and checks**

Run:

```bash
rtk cargo test -p ploy-strategy-bundles -- --nocapture
rtk cargo test -p ploy-research -- --nocapture
rtk cargo check -p ployd
```

Expected: PASS

**Step 6: Commit**

```bash
git add crates/ploy-strategy-bundles crates/ploy-research src/strategy/mod.rs src/cli/strategy/backtest_ops.rs src/strategy/runtime_specs/runtime_plans.rs
git commit -m "strategy: add bundle runtime and isolate research crate"
```

### Task 9: Re-home TUI, Frontend, And Sidecar Onto The Canonical API

**Files:**
- Modify: `src/tui/app.rs`
- Modify: `src/tui/mod.rs`
- Modify: `src/tui/runner.rs`
- Modify: `ploy-frontend/src/pages/SystemControl.tsx`
- Modify: `ploy-frontend/src/pages/StrategyMonitor.tsx`
- Modify: `ploy-frontend/src/services/api.ts`
- Modify: `ploy-frontend/src/services/websocket.ts`
- Modify: `ploy-sidecar/src/index.ts`
- Modify: `ploy-sidecar/src/hooks/risk-guard.ts`
- Test: `src/tui/runner.rs`
- Test: `ploy-frontend/src/services/api.ts`

**Step 1: Write failing contract-alignment tests**

Cover:

- TUI reads deployment snapshots from API instead of local mock state
- frontend stop/pause semantics match control-plane API
- sidecar uses canonical deployment and trading views

**Step 2: Run test to verify failure**

Run the smallest available targeted checks for each surface. If no existing tests exist, add lightweight unit tests around their API adapters first.

**Step 3: Replace local semantics with control-plane semantics**

Make each surface:

- read shared DTOs
- call shared endpoints
- subscribe to shared events

Remove or quarantine:

- demo state
- duplicated risk semantics
- strategy-specific sidecar assumptions

**Step 4: Run tests and build checks**

Run:

```bash
rtk cargo test tui --lib -- --nocapture
cd ploy-frontend && npm run build
cd ploy-sidecar && npm run build
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/tui ploy-frontend/src/pages ploy-frontend/src/services ploy-sidecar/src/index.ts ploy-sidecar/src/hooks/risk-guard.ts
git commit -m "operator: align tui web and sidecar with control plane api"
```

### Task 10: Retire Legacy Bootstrap And Strategy Projection Paths

**Files:**
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/strategy/runtime_specs/deployment_matrix.rs`
- Modify: `src/strategy/runtime_specs/runtime_plans.rs`
- Modify: `src/main_dispatch.rs`
- Modify: `README.md`
- Modify: `docs/STRATEGY_FRAMEWORK_4_PILLARS.md`
- Modify: `tasks/todo.md`
- Test: `src/coordinator/bootstrap.rs`

**Step 1: Write failing retirement tests**

Cover:

- deployment startup no longer depends on strategy-family heuristic classification
- bootstrap is no longer the primary orchestration owner
- docs and task list point to `ployd` + deployment manifests as the default path

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test coordinator::bootstrap --lib -- --nocapture
```

Expected: FAIL because legacy strategy projection logic is still on the hot path.

**Step 3: Remove the old runtime ownership**

Keep only:

- compatibility shims still needed for migration
- explicit deprecation warnings
- pointers to the canonical platform path

Delete or quarantine heuristic runtime projection code once the new deployment contract owns startup.

**Step 4: Run final validation**

Run:

```bash
rtk cargo check -p ployd
rtk cargo check -p ployctl
rtk cargo test -p ploy-platform -- --nocapture
rtk cargo test -p ploy-trading -- --nocapture
rtk cargo test coordinator::bootstrap --lib -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/coordinator/bootstrap.rs src/strategy/runtime_specs/deployment_matrix.rs src/strategy/runtime_specs/runtime_plans.rs src/main_dispatch.rs README.md docs/STRATEGY_FRAMEWORK_4_PILLARS.md tasks/todo.md
git commit -m "architecture: retire legacy bootstrap and projection paths"
```

### Task 11: Add Platform Smoke Coverage And Operator Runbook

**Files:**
- Create: `tests/platform_smoke.rs`
- Modify: `README.md`
- Create: `docs/runbooks/platform-startup.md`
- Create: `config/platform/README.md`
- Create: `config/deployments/README.md`
- Test: `tests/platform_smoke.rs`

**Step 1: Write the failing smoke test**

Cover:

- start one in-memory or test-backed `ployd`
- register one deployment manifest
- supervisor launches one worker
- inspect returns deployment plus trading lifecycle state

**Step 2: Run test to verify failure**

Run:

```bash
rtk cargo test --test platform_smoke -- --nocapture
```

Expected: FAIL because the end-to-end platform path is not wired yet.

**Step 3: Add the minimal smoke harness and docs**

Document the target operator flow:

```bash
ployd start
ployctl deployments apply config/deployments/example.toml
ployctl deployments list
ployctl deployments inspect example
```

Keep the smoke harness narrow. The goal is to prove the platform contract, not to exhaustively test strategy logic.

**Step 4: Run smoke test and final checks**

Run:

```bash
rtk cargo test --test platform_smoke -- --nocapture
rtk cargo check -p ployd
rtk cargo check -p ployctl
```

Expected: PASS

**Step 5: Commit**

```bash
git add tests/platform_smoke.rs README.md docs/runbooks/platform-startup.md config/platform/README.md config/deployments/README.md
git commit -m "docs: add platform smoke coverage and operator runbook"
```
