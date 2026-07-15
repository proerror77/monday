# Strategy Plugin Platform Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a plugin-shaped strategy platform where crypto strategies can be launched from predefined block compositions, event/sports strategies remain registered plugins on the same runtime, and account/order lifecycles explicitly support enable/draining/disable semantics.

**Architecture:** Add a file-backed plugin object model (`definition/spec/deployment`), promote runtime projection to consume plugin objects instead of strategy-specific config, extract an explicit account plane, and extend the order contract with intent purpose so deployment lifecycle can be enforced centrally. The first production slice should make one crypto path run through `ComposableCryptoStrategy`, then bring registered event/sports plugins under the same plugin registry and lifecycle.

**Tech Stack:** Rust, Tokio, Serde/TOML, existing `strategy` / `coordinator` / `platform` modules, Axum API surfaces, SQLx-backed runtime state, targeted unit/integration tests, frequent atomic commits.

---

### Task 1: Add Plugin Object Model And File-Backed Registry Skeleton

**Files:**
- Create: `src/plugins/mod.rs`
- Create: `src/plugins/definition.rs`
- Create: `src/plugins/spec.rs`
- Create: `src/plugins/deployment.rs`
- Create: `src/plugins/registry.rs`
- Modify: `src/lib.rs`
- Create: `config/plugins/README.md`
- Test: `src/plugins/registry.rs`

**Step 1: Write the failing registry tests**

Add tests for:
- loading one `composable_crypto` definition
- loading one `registered_strategy` definition
- rejecting duplicate `plugin_id`
- rejecting unknown `kind`

Use a skeleton like:

```rust
#[test]
fn registry_rejects_duplicate_plugin_ids() {
    let err = PluginRegistry::from_documents(vec![doc_a(), doc_b()]).unwrap_err();
    assert!(err.to_string().contains("duplicate plugin_id"));
}
```

**Step 2: Run test to verify failure**

Run: `cargo test plugins::registry --lib -- --nocapture`
Expected: FAIL because the `plugins` module does not exist yet.

**Step 3: Add minimal plugin model types**

Add neutral types:

```rust
pub enum PluginKind {
    ComposableCrypto,
    RegisteredStrategy,
}

pub struct PluginDefinition {
    pub plugin_id: String,
    pub kind: PluginKind,
    pub version: String,
    pub domain: Domain,
}

pub enum PluginSpec {
    ComposableCrypto(ComposableCryptoSpec),
    RegisteredStrategy(RegisteredStrategySpec),
}

pub enum DeploymentState {
    Enabled,
    Draining,
    Disabled,
    Archived,
}
```

Keep v1 registry file-backed and simple: scan `config/plugins/` TOML files only. Do not add database persistence yet.

**Step 4: Implement minimal registry loader and validation**

- Export `pub mod plugins;` from `src/lib.rs`
- Add registry methods:
  - `PluginRegistry::load_from_dir`
  - `PluginRegistry::plugin`
  - `PluginRegistry::definitions`

**Step 5: Run tests to verify pass**

Run: `cargo test plugins::registry --lib -- --nocapture`
Expected: PASS

**Step 6: Commit**

```bash
git add src/plugins src/lib.rs config/plugins/README.md
git commit -m "plugins: add plugin object model and registry skeleton"
```

### Task 2: Add Deployment Lifecycle State To The Runtime Contract

**Files:**
- Modify: `src/platform/contracts.rs`
- Modify: `src/platform/types.rs`
- Modify: `src/coordinator/coordinator.rs`
- Modify: `src/api/types.rs`
- Test: `src/platform/contracts.rs`
- Test: `src/coordinator/coordinator.rs`

**Step 1: Write failing tests for deployment lifecycle semantics**

Add tests covering:
- `enabled` deployments accept entry intents
- `draining` deployments reject new entries
- `draining` deployments still allow exit/reduce/cancel intents

**Step 2: Run test to verify failure**

Run: `cargo test deployment_runtime_scope --lib -- --nocapture`
Expected: FAIL because deployment state and intent purpose do not exist yet.

**Step 3: Add explicit lifecycle and purpose enums**

Introduce:

```rust
pub enum DeploymentState {
    Enabled,
    Draining,
    Disabled,
    Archived,
}

pub enum IntentPurpose {
    Entry,
    Exit,
    Reduce,
    Hedge,
    Cancel,
}
```

Attach deployment state to the normalized deployment object and intent purpose to the normalized order/intent object.

**Step 4: Implement coordinator gate rules**

In coordinator ingress handling:
- allow all purposes for `Enabled`
- deny `Entry` for `Draining`
- allow `Exit` / `Reduce` / `Hedge` / `Cancel` for `Draining`
- deny all fresh intents for `Disabled` / `Archived`

**Step 5: Run targeted tests**

Run:

```bash
cargo test platform::contracts --lib -- --nocapture
cargo test coordinator --lib -- --nocapture
```

Expected: PASS

**Step 6: Commit**

```bash
git add src/platform/contracts.rs src/platform/types.rs src/coordinator/coordinator.rs src/api/types.rs
git commit -m "platform: add deployment lifecycle and intent purpose contract"
```

### Task 3: Promote `runtime_specs` Into Plugin Projection

**Files:**
- Create: `src/plugins/projector.rs`
- Modify: `src/plugins/mod.rs`
- Modify: `src/coordinator/runtime_specs.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Test: `src/coordinator/bootstrap.rs`

**Step 1: Write failing bootstrap tests**

Cover:
- bootstrap can project a runtime spec from `PluginDefinition + PluginSpec + PluginDeployment`
- no runtime builder callsite imports plugin-specific legacy config types directly

**Step 2: Run test to verify failure**

Run: `cargo test coordinator::bootstrap --lib -- --nocapture`
Expected: FAIL because plugin projection types are not wired into bootstrap.

**Step 3: Add projector abstraction**

Add a neutral projector interface:

```rust
pub struct ProjectedRuntimeSpec {
    pub strategy_label: String,
    pub agent_id: String,
    pub domain: Domain,
    pub strategy_config_toml: String,
}
```

Route current builders through the projector layer instead of calling strategy-specific functions from bootstrap directly.

**Step 4: Keep compatibility wrappers small**

- `src/coordinator/runtime_specs.rs` may temporarily delegate into `src/plugins/projector.rs`
- bootstrap should consume projected specs only

**Step 5: Run tests**

Run:

```bash
cargo test coordinator::bootstrap --lib -- --nocapture
cargo check --bin ploy
```

Expected: PASS

**Step 6: Commit**

```bash
git add src/plugins/projector.rs src/plugins/mod.rs src/coordinator/runtime_specs.rs src/coordinator/bootstrap.rs
git commit -m "plugins: project runtime specs from plugin objects"
```

### Task 4: Extract Account Plane Skeleton And Re-Home Claimer Ownership

**Files:**
- Create: `src/account/mod.rs`
- Create: `src/account/registry.rs`
- Create: `src/account/budget.rs`
- Create: `src/account/ledger.rs`
- Create: `src/account/service.rs`
- Create: `src/account/claimer.rs`
- Modify: `src/lib.rs`
- Modify: `src/strategy/claimer.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/api/handlers/system.rs`
- Test: `src/account/service.rs`

**Step 1: Write failing tests for account service behavior**

Cover:
- account service resolves one runtime account from config + registry rows
- account service exposes a claimer handle without importing it from `strategy::*`
- account service returns deployment coverage and budget snapshot together

**Step 2: Run test to verify failure**

Run: `cargo test account::service --lib -- --nocapture`
Expected: FAIL because `account` module does not exist yet.

**Step 3: Add account-plane skeleton**

Add neutral types:

```rust
pub struct AccountSnapshot {
    pub account_id: String,
    pub wallet_address: Option<String>,
    pub deployment_total: usize,
    pub deployment_enabled: usize,
}

pub struct AccountBudgetSnapshot {
    pub available_notional_usd: Decimal,
    pub reserved_notional_usd: Decimal,
}
```

**Step 4: Re-home claimer ownership**

- keep implementation temporarily delegating to current claimer internals
- expose account-facing claimer handle from `src/account/claimer.rs`
- update bootstrap/API reads to go through account service rather than treating claimer as a strategy concern

**Step 5: Run tests**

Run:

```bash
cargo test account::service --lib -- --nocapture
cargo test coordinator::bootstrap --lib -- --nocapture
```

Expected: PASS

**Step 6: Commit**

```bash
git add src/account src/lib.rs src/strategy/claimer.rs src/coordinator/bootstrap.rs src/api/handlers/system.rs
git commit -m "account: add account plane skeleton and re-home claimer ownership"
```

### Task 5: Add Intent Purpose To Canonical Strategy Actions

**Files:**
- Modify: `src/strategy/traits.rs`
- Modify: `src/strategy/manager.rs`
- Modify: `src/coordinator/strategy_runtime.rs`
- Modify: `src/platform/contracts.rs`
- Test: `src/coordinator/strategy_runtime.rs`
- Test: `src/strategy/manager.rs`

**Step 1: Write failing runtime tests**

Cover:
- `StrategyAction::SubmitOrder` carries purpose
- managed runtime forwards the correct purpose into coordinator/order contract
- `CancelOrder` still works during draining

**Step 2: Run test to verify failure**

Run:

```bash
cargo test strategy::manager --lib -- --nocapture
cargo test coordinator::strategy_runtime --lib -- --nocapture
```

Expected: FAIL because `StrategyAction::SubmitOrder` does not include purpose yet.

**Step 3: Extend canonical strategy contract**

Add:

```rust
pub enum OrderPurpose {
    Entry,
    Exit,
    Reduce,
    Hedge,
}
```

and attach it to `StrategyAction::SubmitOrder`.

**Step 4: Propagate purpose end-to-end**

- strategy manager action channel
- managed runtime order execution bridge
- normalized order intent / command / execution reporting

**Step 5: Run targeted tests**

Run:

```bash
cargo test strategy::manager --lib -- --nocapture
cargo test coordinator::strategy_runtime --lib -- --nocapture
cargo test coordinator::bootstrap --lib -- --nocapture
```

Expected: PASS

**Step 6: Commit**

```bash
git add src/strategy/traits.rs src/strategy/manager.rs src/coordinator/strategy_runtime.rs src/platform/contracts.rs
git commit -m "strategy: add order purpose to canonical strategy actions"
```

### Task 6: Build `ComposableCryptoSpec` And Block Catalog

**Files:**
- Create: `src/plugins/composable_crypto/mod.rs`
- Create: `src/plugins/composable_crypto/blocks.rs`
- Create: `src/plugins/composable_crypto/schema.rs`
- Modify: `src/plugins/spec.rs`
- Test: `src/plugins/composable_crypto/schema.rs`

**Step 1: Write failing schema tests**

Cover:
- a valid spec with one signal, one filter, one entry, one exit, one sizing block parses
- unknown block type is rejected
- duplicate singleton sections (for example two `entry` blocks) are rejected

**Step 2: Run test to verify failure**

Run: `cargo test plugins::composable_crypto --lib -- --nocapture`
Expected: FAIL because the composable crypto schema does not exist.

**Step 3: Add the v1 block catalog**

Support only:
- signals: `momentum`, `mean_reversion`, `spread_dislocation`
- filters: `time_window`, `volatility_gate`, `liquidity_gate`
- entry: `marketable_limit`, `ladder_limit`
- exit: `trailing_stop`, `edge_decay`, `time_stop`
- sizing: `fixed_shares`, `fixed_usd_risk`, `budget_fraction`

**Step 4: Implement schema validation**

- require at least one signal
- require exactly one entry, one exit, one sizing block
- reject unknown block names early

**Step 5: Run tests**

Run: `cargo test plugins::composable_crypto --lib -- --nocapture`
Expected: PASS

**Step 6: Commit**

```bash
git add src/plugins/composable_crypto src/plugins/spec.rs
git commit -m "plugins: add composable crypto spec and block catalog"
```

### Task 7: Implement `ComposableCryptoStrategy` And Migrate One Crypto Path

**Files:**
- Create: `src/strategy/composable_crypto.rs`
- Modify: `src/strategy/mod.rs`
- Modify: `src/strategy/manager.rs`
- Modify: `src/plugins/projector.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Test: `src/strategy/composable_crypto.rs`
- Test: `src/coordinator/bootstrap.rs`

**Step 1: Write failing integration tests**

Cover:
- one composable crypto plugin deployment boots through canonical runtime
- a basic momentum + volatility_gate + trailing_stop spec emits entry then exit intents
- no direct strategy-specific builder is required for the migrated crypto path

**Step 2: Run test to verify failure**

Run:

```bash
cargo test strategy::composable_crypto --lib -- --nocapture
cargo test coordinator::bootstrap --lib -- --nocapture
```

Expected: FAIL because `ComposableCryptoStrategy` does not exist.

**Step 3: Implement the runtime**

- parse `ComposableCryptoSpec`
- instantiate block objects from registry
- execute blocks in fixed order:
  - signals
  - filters
  - entry/exit decision
  - sizing

**Step 4: Migrate one representative path first**

Use one existing crypto managed path as the first adopter. Recommended: `momentum`.

Do not migrate `split_arb` in the same commit; keep the first slice small.

**Step 5: Run tests**

Run:

```bash
cargo test strategy::composable_crypto --lib -- --nocapture
cargo test coordinator::bootstrap --lib -- --nocapture
cargo check --bin ploy
```

Expected: PASS

**Step 6: Commit**

```bash
git add src/strategy/composable_crypto.rs src/strategy/mod.rs src/strategy/manager.rs src/plugins/projector.rs src/coordinator/bootstrap.rs
git commit -m "strategy: add composable crypto runtime and migrate momentum path"
```

### Task 8: Convert Event/Sports To Registered Plugins On The Same Registry

**Files:**
- Modify: `src/plugins/spec.rs`
- Modify: `src/plugins/registry.rs`
- Modify: `src/strategy/manager.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/strategy/event_edge/strategy.rs`
- Modify: `src/strategy/nba_comeback/strategy.rs`
- Test: `src/coordinator/bootstrap.rs`
- Test: `src/strategy/event_edge/strategy.rs`
- Test: `src/strategy/nba_comeback/strategy.rs`

**Step 1: Write failing tests**

Cover:
- `event_edge` can be addressed via plugin registry lookup
- `nba_comeback` can be addressed via plugin registry lookup
- both deployments still start through canonical runtime
- neither path bypasses plugin deployment lifecycle gating

**Step 2: Run test to verify failure**

Run:

```bash
cargo test strategy::event_edge --lib -- --nocapture
cargo test strategy::nba_comeback --lib -- --nocapture
cargo test coordinator::bootstrap --lib -- --nocapture
```

Expected: FAIL because registered plugin definitions are not wired into the registry.

**Step 3: Register strategy-backed plugins**

Keep code-backed implementations, but make bootstrap resolve them through `PluginDefinition` and `PluginDeployment` rather than bespoke strategy selection.

**Step 4: Run tests**

Run:

```bash
cargo test strategy::event_edge --lib -- --nocapture
cargo test strategy::nba_comeback --lib -- --nocapture
cargo test coordinator::bootstrap --lib -- --nocapture
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/plugins/spec.rs src/plugins/registry.rs src/strategy/manager.rs src/coordinator/bootstrap.rs src/strategy/event_edge/strategy.rs src/strategy/nba_comeback/strategy.rs
git commit -m "plugins: register event and sports strategies in plugin registry"
```

### Task 9: Expose Plugin And Account Lifecycles Through API And Docs

**Files:**
- Modify: `src/api/types.rs`
- Modify: `src/api/handlers/system.rs`
- Modify: `src/api/routes.rs`
- Modify: `README.md`
- Modify: `docs/STRATEGY_FRAMEWORK_4_PILLARS.md`
- Modify: `docs/plans/2026-03-07-strategy-plugin-platform-design.md`
- Modify: `tasks/todo.md`
- Test: `src/api/handlers/system.rs`

**Step 1: Write failing API tests**

Cover:
- account overview includes deployment state counts
- plugin lifecycle state is visible through API responses
- draining deployments appear as active-but-draining, not disabled

**Step 2: Run test to verify failure**

Run: `cargo test api::handlers::system --lib -- --nocapture`
Expected: FAIL because plugin/account lifecycle fields are not exposed.

**Step 3: Add the minimal API surface**

Expose:
- plugin deployment state
- account budget/coverage snapshot
- runtime/plugin capability summary

Do not add a full CRUD plugin API in this cut; read-side visibility is enough.

**Step 4: Run tests**

Run:

```bash
cargo test api::handlers::system --lib -- --nocapture
cargo check --features api --bin ploy
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/api/types.rs src/api/handlers/system.rs src/api/routes.rs README.md docs/STRATEGY_FRAMEWORK_4_PILLARS.md docs/plans/2026-03-07-strategy-plugin-platform-design.md tasks/todo.md
git commit -m "api: expose plugin and account lifecycle state"
```

### Task 10: Retire Obsolete Strategy Startup Surfaces

**Files:**
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/coordinator/runtime_specs.rs`
- Modify: `src/agents/mod.rs`
- Modify: `src/platform/mod.rs`
- Modify: `src/main_modes/platform_mode.rs`
- Test: `src/coordinator/bootstrap.rs`
- Test: `src/main_modes/platform_mode.rs`

**Step 1: Write failing tests**

Cover:
- crypto plugin deployments no longer require strategy-family-specific bootstrap branching
- runtime startup is plugin/deployment-driven
- platform mode reflects plugin/deployment selection instead of old runtime surfaces

**Step 2: Run test to verify failure**

Run:

```bash
cargo test coordinator::bootstrap --lib -- --nocapture
cargo test main_modes::platform_mode --lib -- --nocapture
```

Expected: FAIL because bootstrap still contains legacy strategy/startup assumptions.

**Step 3: Remove obsolete startup wiring**

- keep plugin registry + projector + canonical runtime only
- shrink leftover strategy-specific startup helpers
- preserve compatibility shims only if another active plan still depends on them

**Step 4: Run final validation**

Run:

```bash
cargo test coordinator::bootstrap --lib -- --nocapture
cargo test main_modes::platform_mode --lib -- --nocapture
cargo check --bin ploy
cargo check --features api --bin ploy
```

Expected: PASS

**Step 5: Commit**

```bash
git add src/coordinator/bootstrap.rs src/coordinator/runtime_specs.rs src/agents/mod.rs src/platform/mod.rs src/main_modes/platform_mode.rs
git commit -m "architecture: route startup through plugin deployments"
```
