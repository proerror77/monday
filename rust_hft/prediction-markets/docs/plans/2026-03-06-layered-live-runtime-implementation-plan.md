# Layered Live Runtime Refactor Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Converge live trading onto one canonical strategy runtime, move OpenClaw-style logic into a dedicated capital-governance plane, and reduce `bootstrap` back to pure assembly.

**Architecture:** Keep coordinator as the sole live execution ingress. Shrink the `Strategy` contract into a pure decision-layer interface, extract the managed strategy runtime out of `bootstrap`, then migrate existing live strategies onto that runtime in controlled waves while governance is split into a separate policy path.

**Tech Stack:** Rust, Tokio, Axum, SQLx, existing coordinator/strategy/platform modules, targeted unit/integration tests.

---

### Task 1: Freeze Canonical Ownership In Docs And Module Boundaries

**Files:**
- Modify: `src/strategy/traits.rs`
- Modify: `src/agents/traits.rs`
- Modify: `src/platform/traits.rs`
- Modify: `src/strategy/mod.rs`
- Modify: `docs/plans/2026-03-06-layered-live-runtime-design.md`

**Steps:**
1. Add module-level docs marking `Strategy` as the only canonical live strategy contract.
2. Add deprecation/transitional comments to `TradingAgent` and `DomainAgent` stating they are compatibility paths and must not be used for new live strategies.
3. Add a short architecture comment in `src/strategy/mod.rs` pointing new live strategy work to the canonical runtime path.
4. Run: `cargo check --lib`
Expected: PASS
5. Commit:

```bash
git add src/strategy/traits.rs src/agents/traits.rs src/platform/traits.rs src/strategy/mod.rs docs/plans/2026-03-06-layered-live-runtime-design.md
git commit -m "architecture: freeze canonical live runtime ownership"
```

### Task 2: Shrink The Strategy Contract To Pure Decision Responsibilities

**Files:**
- Modify: `src/strategy/traits.rs`
- Modify: `src/strategy/manager.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Test: `src/strategy/manager.rs`

**Steps:**
1. Remove `UpdateRisk`, `SubscribeFeed`, and `UnsubscribeFeed` from `StrategyAction`.
2. Replace any runtime wiring expectations with static `required_feeds()` only.
3. Update `StrategyManager` action handling to reject or delete now-obsolete branches.
4. Write or update focused tests covering:
   - market update -> order action flow still works
   - order update -> state transition still works
   - no strategy path can request feed rewiring or governance mutation
5. Run: `cargo test strategy::manager --lib -- --nocapture`
Expected: PASS
6. Commit:

```bash
git add src/strategy/traits.rs src/strategy/manager.rs src/coordinator/bootstrap.rs
git commit -m "strategy: remove governance and feed control from strategy contract"
```

### Task 3: Extract A Canonical Managed Strategy Runtime Module

**Files:**
- Create: `src/coordinator/strategy_runtime.rs`
- Modify: `src/coordinator/mod.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Test: `src/coordinator/bootstrap.rs`

**Steps:**
1. Move `run_managed_strategy_runtime` and its immediate helper logic out of `bootstrap.rs` into `src/coordinator/strategy_runtime.rs`.
2. Keep the runtime API generic:
   - build strategy from registry/factory
   - connect feeds
   - bridge strategy outputs into coordinator execution ingress
   - process coordinator commands and shutdown
3. Leave `bootstrap.rs` only responsible for calling the runtime with deployment/runtime inputs.
4. Add a targeted regression test proving bootstrap no longer needs strategy-specific runtime internals to start a managed strategy.
5. Run: `cargo test coordinator::bootstrap --lib -- --nocapture`
Expected: PASS
6. Commit:

```bash
git add src/coordinator/strategy_runtime.rs src/coordinator/mod.rs src/coordinator/bootstrap.rs
git commit -m "coordinator: extract canonical managed strategy runtime"
```

### Task 4: Introduce A Governance-Only Context For OpenClaw Policy

**Files:**
- Create: `src/agents/governance_context.rs`
- Modify: `src/agents/context.rs`
- Modify: `src/agents/openclaw/agent.rs`
- Modify: `src/coordinator/coordinator.rs`
- Test: `src/coordinator/coordinator.rs`

**Steps:**
1. Split agent-facing coordinator access into:
   - trading context with `submit_order`
   - governance context with pause/resume/policy update/read-only state access
2. Update OpenClaw to use the governance-only context and remove any direct order-submission ability.
3. Add tests proving governance agents can:
   - pause/resume strategies
   - update governance policy
   - read portfolio/global state
   - not submit trading intents
4. Run: `cargo test coordinator --lib -- --nocapture`
Expected: PASS
5. Commit:

```bash
git add src/agents/governance_context.rs src/agents/context.rs src/agents/openclaw/agent.rs src/coordinator/coordinator.rs
git commit -m "governance: separate policy context from trading context"
```

### Task 5: Move Strategy Risk Ownership To Governance/Deployment Policy

**Files:**
- Modify: `src/platform/traits.rs`
- Modify: `src/agents/traits.rs`
- Modify: `src/platform/risk.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Test: `src/platform/risk.rs`

**Steps:**
1. Stop treating per-strategy runtime types as the owner of `AgentRiskParams`.
2. Move risk-parameter binding to deployment/bootstrap/governance projection.
3. Remove self-governing pause behavior from strategy runtimes where possible; convert repeated failure reactions into signals/health that governance can consume.
4. Add tests proving the risk gate still receives agent/domain parameters from coordinator/bootstrap even after strategy runtime ownership is removed.
5. Run: `cargo test platform::risk --lib -- --nocapture`
Expected: PASS
6. Commit:

```bash
git add src/platform/traits.rs src/agents/traits.rs src/platform/risk.rs src/coordinator/bootstrap.rs
git commit -m "governance: move live risk ownership out of strategy runtimes"
```

### Task 6: Migrate `split_arb` And `momentum` Fully Onto The Canonical Runtime

**Files:**
- Modify: `src/strategy/adapters.rs`
- Modify: `src/strategy/manager.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/agents/crypto.rs`
- Test: `src/strategy/adapters.rs`
- Test: `src/coordinator/bootstrap.rs`

**Steps:**
1. Make managed `split_arb/staggered_arb` and `momentum` start only through the canonical strategy runtime.
2. Remove the bootstrap branch that starts crypto momentum through `CryptoTradingAgent`.
3. Keep `src/agents/crypto.rs` only as temporary compatibility scaffolding, with clear deprecation notes if any code remains.
4. Add/update tests proving deployment enablement starts the canonical strategy runtime for both strategies.
5. Run:

```bash
cargo test build_split_arb_runtime_config_ --lib -- --nocapture
cargo test strategy::adapters --lib -- --nocapture
```

Expected: PASS
6. Commit:

```bash
git add src/strategy/adapters.rs src/strategy/manager.rs src/coordinator/bootstrap.rs src/agents/crypto.rs
git commit -m "strategy: migrate momentum and split arb to canonical runtime"
```

### Task 7: Migrate `event_edge` And `nba_comeback` Into The Strategy Plane

**Files:**
- Create: `src/strategy/event_edge/strategy.rs`
- Create: `src/strategy/nba_comeback/strategy.rs`
- Modify: `src/strategy/manager.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/agents/politics.rs`
- Modify: `src/agents/sports.rs`
- Test: `src/strategy/event_edge/strategy.rs`
- Test: `src/strategy/nba_comeback/strategy.rs`

**Steps:**
1. Wrap `EventEdgeCore` in a canonical `Strategy` implementation.
2. Wrap `NbaComebackCore` in a canonical `Strategy` implementation.
3. Register both in `StrategyFactory`.
4. Change bootstrap to start politics/sports live strategy deployments through the canonical strategy runtime instead of `PoliticsTradingAgent`/`SportsTradingAgent`.
5. Keep the old agent files only as temporary adapters or delete them if no longer used.
6. Add targeted tests proving both strategies can:
   - accept runtime updates
   - emit normalized strategy actions/intents
   - handle execution callbacks without direct coordinator ownership
7. Run:

```bash
cargo test strategy::event_edge --lib -- --nocapture
cargo test strategy::nba_comeback --lib -- --nocapture
```

Expected: PASS
8. Commit:

```bash
git add src/strategy/event_edge/strategy.rs src/strategy/nba_comeback/strategy.rs src/strategy/manager.rs src/coordinator/bootstrap.rs src/agents/politics.rs src/agents/sports.rs
git commit -m "strategy: migrate event edge and nba comeback to canonical runtime"
```

### Task 8: Retire Parallel Live Runtime Entry Points

**Files:**
- Modify: `src/platform/mod.rs`
- Modify: `src/platform/agents/mod.rs`
- Modify: `src/agents/mod.rs`
- Modify: `src/main_modes/platform_mode.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Test: `src/main_modes/platform_mode.rs`

**Steps:**
1. Remove platform-agent startup branches for live strategy execution.
2. Remove or gate pull-based trading-agent startup branches for live strategy execution.
3. Keep only governance agents and canonical strategy runtime registration paths.
4. Update platform-mode tests so they assert canonical runtime selection instead of legacy/pull-based branches.
5. Run: `cargo test main_modes::platform_mode --lib -- --nocapture`
Expected: PASS
6. Commit:

```bash
git add src/platform/mod.rs src/platform/agents/mod.rs src/agents/mod.rs src/main_modes/platform_mode.rs src/coordinator/bootstrap.rs
git commit -m "architecture: retire parallel live runtime entry points"
```

### Task 9: Reduce Bootstrap To Pure Assembly And Delete Strategy Special Cases

**Files:**
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/coordinator/strategy_runtime.rs`
- Modify: `src/strategy/manager.rs`
- Test: `src/coordinator/bootstrap.rs`

**Steps:**
1. Delete strategy-name classification logic from bootstrap.
2. Delete strategy-specific TOML rendering from bootstrap.
3. Delete strategy-specific execution observability branches from bootstrap.
4. Replace them with registry-driven runtime startup and generic runtime hooks.
5. Add/update regression tests proving bootstrap no longer needs strategy-name branching for supported live strategies.
6. Run: `cargo test coordinator::bootstrap --lib -- --nocapture`
Expected: PASS
7. Commit:

```bash
git add src/coordinator/bootstrap.rs src/coordinator/strategy_runtime.rs src/strategy/manager.rs
git commit -m "coordinator: reduce bootstrap to pure assembly"
```

### Task 10: Final Validation, Docs, And Cleanup

**Files:**
- Modify: `README.md`
- Modify: `docs/STRATEGY_FRAMEWORK_4_PILLARS.md`
- Modify: `docs/plans/2026-03-06-layered-live-runtime-design.md`
- Modify: `tasks/todo.md`

**Steps:**
1. Update docs to describe the new four-layer canonical runtime.
2. Remove outdated references to legacy multi-runtime coexistence as the intended steady state.
3. Run the smallest realistic validation set for migrated live strategies.
4. Review staged diff path-by-path.
5. Commit:

```bash
git add README.md docs/STRATEGY_FRAMEWORK_4_PILLARS.md docs/plans/2026-03-06-layered-live-runtime-design.md tasks/todo.md
git commit -m "docs: document canonical layered live runtime"
```
