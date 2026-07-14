# Legacy Runtime Retirement Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Remove the remaining legacy live-runtime surfaces (`TradingAgent`, `DomainAgent`, compatibility bootstrap branches, and env-gated fallback runtimes) now that the canonical layered runtime is in place.

**Architecture:** The layered-runtime refactor is effectively complete. What remains is not another design pass; it is retirement work. The safe sequence is: move config ownership out of legacy runtime files, verify no production dependence on compatibility flags, delete pull-based compatibility runtimes, then remove the platform-agent compatibility layer and its traits.

**Tech Stack:** Rust, Clap, Tokio, SQLx, existing `coordinator` / `strategy` / `agents` / `platform` modules, targeted unit tests, `rg`-based inventory checks, and deployment/env grep verification.

---

## Current Assessment

There is no large architectural redesign left to do on this branch.

What is still open falls into two buckets:

1. **Operational verification, not architecture**
   - `STAG-ARB` post-restart live acceptance still needs a real `LEG1 -> LEG2` path.
   - Those checks should stay outside the code-deletion PR and be tracked separately.

2. **Code retirement**
   - `src/agents/*` still contains pull-based compatibility runtimes.
   - `src/platform/agents/*` still contains `DomainAgent` compatibility implementations.
   - `src/coordinator/bootstrap.rs` still contains opt-in compatibility startup branches and env gates.
   - `src/coordinator/runtime_specs.rs` still imports config types from legacy runtime modules, which blocks full deletion.

This plan is only about bucket 2.

## Inventory Snapshot (2026-03-08)

The live retirement scope is now frozen to these remaining surfaces.

### Delete now

- `README.md` compatibility-flag notes for `PLOY_ENABLE_COMPAT_CRYPTO_RUNTIMES`
  and `PLOY_ENABLE_COMPAT_SPORTS_RUNTIMES`
- `docs/STRATEGY_FRAMEWORK_4_PILLARS.md` steady-state text that still treats those
  env gates as active operator-facing surfaces

Historical docs that mention `TradingAgent` / `DomainAgent` stay until Task 7 so
the code-deletion commits remain narrowly scoped.

### Delete after config extraction

- `src/coordinator/runtime_specs.rs` imports `PoliticsTradingConfig` /
  `SportsTradingConfig` from legacy runtime modules
- `src/plugins/projector.rs` imports `PoliticsTradingConfig` /
  `SportsTradingConfig` from legacy runtime modules
- `src/coordinator/bootstrap.rs` still owns:
  - `spawn_compat_trading_agent_task(...)`
  - `spawn_compat_crypto_lob_ml_agent(...)`
  - `maybe_spawn_compat_crypto_lob_ml_agent(...)`
  - `spawn_compat_crypto_rl_policy_agent(...)`
  - `maybe_spawn_compat_crypto_rl_policy_agent(...)`
  - `PLOY_ENABLE_COMPAT_CRYPTO_RUNTIMES`
  - `PLOY_ENABLE_COMPAT_SPORTS_RUNTIMES`
- `src/agents/politics.rs`
- `src/agents/sports.rs`
- `src/agents/crypto_lob_ml.rs`
- `src/agents/crypto_rl_policy.rs`

### Delete after RL/CLI migration

- `src/agents/traits.rs` `TradingAgent`
- `src/platform/traits.rs` `DomainAgent` / `SimpleAgent`
- `src/platform/platform.rs` and `src/platform/router.rs` `DomainAgent` callsites
- `src/platform/agents/{crypto_agent,event_edge_agent,nba_agent,rl_crypto_agent}.rs`
- `src/cli/strategy.rs` `NbaComebackAgent` compatibility CLI path
- `src/main_commands/rl/agent.rs` `RLCryptoAgent` / `DomainAgent` imports

### Task 1: Freeze The Retirement Scope With A Grep-Based Inventory

**Files:**
- Modify: `tasks/todo.md`
- Modify: `docs/plans/2026-03-07-legacy-runtime-retirement-plan.md`

**Steps:**
1. Run:

```bash
rg -n "TradingAgent|DomainAgent|PLOY_ENABLE_COMPAT|spawn_compat_|platform::agents|agents::sports|agents::politics|agents::crypto_lob_ml|agents::crypto_rl_policy" src README.md docs
```

2. Copy the exact remaining callsites into `tasks/todo.md` under a new retirement section.
3. Split the inventory into:
   - delete now
   - delete after config extraction
   - delete after RL/CLI migration
4. Commit:

```bash
git add tasks/todo.md docs/plans/2026-03-07-legacy-runtime-retirement-plan.md
git commit -m "docs: freeze legacy runtime retirement scope"
```

### Task 2: Move Runtime Config Ownership Out Of Legacy Agent Files

**Files:**
- Modify: `src/config.rs`
- Modify: `src/coordinator/runtime_specs.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/agents/crypto.rs`
- Modify: `src/agents/politics.rs`
- Modify: `src/agents/sports.rs`
- Test: `src/coordinator/bootstrap.rs`

**Steps:**
1. Write a failing bootstrap/config test proving runtime spec builders no longer need to import config types from legacy runtime modules.
2. Run it to verify failure.
3. Move `CryptoTradingConfig`, `PoliticsTradingConfig`, and `SportsTradingConfig` into `src/config.rs` (or another neutral config location inside `src/config.rs` if decomposition is still deferred).
4. Update `runtime_specs.rs` and `bootstrap.rs` to import those neutral config types instead of `src/agents/*`.
5. Leave temporary type aliases in the legacy modules only if needed to keep the diff small for one cut.
6. Run:

```bash
cargo test coordinator::bootstrap --lib -- --nocapture
cargo check --bin ploy
```

7. Commit:

```bash
git add src/config.rs src/coordinator/runtime_specs.rs src/coordinator/bootstrap.rs src/agents/crypto.rs src/agents/politics.rs src/agents/sports.rs
git commit -m "config: decouple runtime specs from legacy agent modules"
```

### Task 3: Delete Politics/Sports Compatibility Runtime Branches

**Files:**
- Modify: `src/coordinator/bootstrap.rs`
- Delete: `src/agents/politics.rs`
- Modify: `src/agents/sports.rs`
- Modify: `README.md`
- Modify: `docs/STRATEGY_FRAMEWORK_4_PILLARS.md`
- Test: `src/coordinator/bootstrap.rs`

**Steps:**
1. Confirm there is no deployment/env/operator requirement for `PLOY_ENABLE_COMPAT_SPORTS_RUNTIMES`.
2. Write a failing bootstrap test asserting sports and politics start only through the canonical managed runtime path.
3. Run it to verify failure.
4. Remove the politics compatibility runtime entirely.
5. Remove the Grok-enabled sports fallback branch from `bootstrap.rs` instead of carrying it forward as special legacy logic.
6. Either:
   - delete `src/agents/sports.rs` outright if no non-runtime code remains, or
   - shrink it to non-runtime helpers only in a follow-up cut if a full delete is too large.
7. Update docs to remove the sports compatibility env flag.
8. Run:

```bash
cargo test coordinator::bootstrap --lib -- --nocapture
cargo check --bin ploy
```

9. Commit:

```bash
git add src/coordinator/bootstrap.rs src/agents/sports.rs README.md docs/STRATEGY_FRAMEWORK_4_PILLARS.md
git rm src/agents/politics.rs
git commit -m "architecture: remove politics and sports compatibility runtimes"
```

### Task 4: Delete Crypto Compatibility Runtime Branches

**Files:**
- Modify: `src/coordinator/bootstrap.rs`
- Delete: `src/agents/crypto_lob_ml.rs`
- Delete: `src/agents/crypto_rl_policy.rs`
- Modify: `src/agents/mod.rs`
- Modify: `README.md`
- Modify: `docs/STRATEGY_FRAMEWORK_4_PILLARS.md`
- Test: `src/coordinator/bootstrap.rs`

**Steps:**
1. Confirm there is no production/operator dependency on `PLOY_ENABLE_COMPAT_CRYPTO_RUNTIMES`.
2. Write a failing bootstrap test asserting the platform no longer exposes `lob_ml` / `rl_policy` compatibility startup paths.
3. Run it to verify failure.
4. Remove:
   - `spawn_compat_trading_agent_task(...)`
   - `spawn_compat_crypto_lob_ml_agent(...)`
   - `maybe_spawn_compat_crypto_lob_ml_agent(...)`
   - `spawn_compat_crypto_rl_policy_agent(...)`
   - `maybe_spawn_compat_crypto_rl_policy_agent(...)`
   - both compatibility env-gate helpers
5. Delete the now-unused runtime modules and module exports.
6. Update docs to remove the crypto compatibility env flag.
7. Run:

```bash
cargo test coordinator::bootstrap --lib -- --nocapture
cargo check --bin ploy
cargo check --features rl --bin ploy
```

8. Commit:

```bash
git add src/coordinator/bootstrap.rs src/agents/mod.rs README.md docs/STRATEGY_FRAMEWORK_4_PILLARS.md
git rm src/agents/crypto_lob_ml.rs src/agents/crypto_rl_policy.rs
git commit -m "architecture: remove crypto compatibility runtimes"
```

### Task 5: Remove The `TradingAgent` Compatibility Trait

**Files:**
- Modify: `src/agents/traits.rs`
- Modify: `src/agents/mod.rs`
- Modify: `src/coordinator/bootstrap.rs`
- Modify: `src/agents/context.rs`
- Test: `src/coordinator/bootstrap.rs`

**Steps:**
1. Write a failing compile/test check proving no runtime path still requires `TradingAgent`.
2. Run it to verify failure.
3. Delete the `TradingAgent` trait and any helper code that only exists to support pull-based runtime startup.
4. Keep `GovernanceAgent` intact; move it to a clearer ownership location only if the diff stays small.
5. Re-run bootstrap tests and a full compile.
6. Run:

```bash
cargo test coordinator::bootstrap --lib -- --nocapture
cargo check --bin ploy
```

7. Commit:

```bash
git add src/agents/traits.rs src/agents/mod.rs src/coordinator/bootstrap.rs src/agents/context.rs
git commit -m "architecture: remove trading-agent compatibility contract"
```

### Task 6: Remove The `DomainAgent` / `platform::agents` Compatibility Layer

**Files:**
- Modify: `src/platform/traits.rs`
- Modify: `src/platform/mod.rs`
- Delete: `src/platform/agents/crypto_agent.rs`
- Delete: `src/platform/agents/event_edge_agent.rs`
- Delete: `src/platform/agents/nba_agent.rs`
- Delete: `src/platform/agents/rl_crypto_agent.rs`
- Modify: `src/platform/agents/mod.rs`
- Modify: `src/platform/platform.rs`
- Modify: `src/platform/router.rs`
- Modify: `src/cli/strategy.rs`
- Modify: `src/main_commands/rl/agent.rs`
- Test: `src/main_modes/platform_mode.rs`

**Steps:**
1. Grep all remaining `DomainAgent` callsites and classify them into:
   - delete
   - migrate to canonical strategy runtime
   - migrate to explicit RL/research-only command path
2. Write a failing compile/test proving `platform_mode` and strategy CLI no longer depend on `platform::agents::*`.
3. Run it to verify failure.
4. Delete the platform-agent runtime implementations.
5. Remove `DomainAgent` and `SimpleAgent` from `src/platform/traits.rs` once no callsites remain.
6. If RL tooling still needs runtime helpers, move them into a feature-scoped command module instead of keeping the old platform-agent shape.
7. Run:

```bash
cargo test main_modes::platform_mode --lib -- --nocapture
cargo check --bin ploy
cargo check --features rl --bin ploy
```

8. Commit:

```bash
git add src/platform/traits.rs src/platform/mod.rs src/platform/agents/mod.rs src/platform/platform.rs src/platform/router.rs src/cli/strategy.rs src/main_commands/rl/agent.rs
git rm src/platform/agents/crypto_agent.rs src/platform/agents/event_edge_agent.rs src/platform/agents/nba_agent.rs src/platform/agents/rl_crypto_agent.rs
git commit -m "architecture: remove domain-agent compatibility layer"
```

### Task 7: Final Cleanup, Docs, And Proof

**Files:**
- Modify: `README.md`
- Modify: `docs/STRATEGY_FRAMEWORK_4_PILLARS.md`
- Modify: `docs/plans/2026-03-06-layered-live-runtime-design.md`
- Modify: `tasks/todo.md`

**Steps:**
1. Run:

```bash
rg -n "TradingAgent|DomainAgent|PLOY_ENABLE_COMPAT|platform::agents|spawn_compat_" src README.md docs
```

2. Confirm the remaining matches are historical docs or intentional mentions only.
3. Update docs to describe the new steady state:
   - canonical `Strategy` runtime only
   - governance-only OpenClaw
   - no compatibility env flags
4. Mark the legacy-runtime retirement tasks complete in `tasks/todo.md`.
5. Run the final validation set:

```bash
cargo test coordinator::bootstrap --lib -- --nocapture
cargo test main_modes::platform_mode --lib -- --nocapture
cargo check --bin ploy
cargo check --features rl --bin ploy
```

6. Commit:

```bash
git add README.md docs/STRATEGY_FRAMEWORK_4_PILLARS.md docs/plans/2026-03-06-layered-live-runtime-design.md tasks/todo.md
git commit -m "docs: finalize legacy runtime retirement"
```

## Recommended Execution Order

1. Task 2 first: unblock deletion by moving config ownership.
2. Task 3 and Task 4 next: remove sports/politics and crypto compatibility runtimes.
3. Task 5 after that: delete the `TradingAgent` abstraction.
4. Task 6 last: delete `DomainAgent` and `platform::agents/*` only after CLI/RL callsites are settled.
5. Keep `STAG-ARB` live acceptance in a separate operational checklist; do not mix it into the deletion PR.

## Acceptance Criteria

1. `src/coordinator/bootstrap.rs` contains no `spawn_compat_*` runtime branches.
2. No live startup path depends on `TradingAgent` or `DomainAgent`.
3. `README.md` and `docs/STRATEGY_FRAMEWORK_4_PILLARS.md` no longer mention compatibility env flags.
4. `src/agents/politics.rs`, `src/agents/crypto_lob_ml.rs`, `src/agents/crypto_rl_policy.rs`, and `src/platform/agents/*` are deleted or reduced to non-runtime code with explicit justification.
5. Any RL or research-only runtime helpers have an explicit non-live ownership path.
6. The only remaining open items after the retirement plan are operational live validations, not architecture ownership cleanup.
