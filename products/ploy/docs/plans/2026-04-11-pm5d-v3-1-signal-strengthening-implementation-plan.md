# PM5D V3.1 Signal Strengthening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a narrow V3-only trend-persistence and weak-signal filter to the
existing directional strategy without changing runtime/data-plane boundaries.

**Architecture:** Keep all logic inside `directional.rs` and the V3 config
files. Reuse the existing `ReturnBuffer`, add two additive config fields, and
validate behavior with direct strategy tests before touching production logic.

**Tech Stack:** Rust, existing `DirectionalStrategy` unit tests, checked-in V3
strategy TOML.

---

### Task 1: Record the config surface and task ownership

**Files:**
- Modify: `tasks/todo.md`
- Modify: `docs/plans/2026-04-11-pm5d-v3-1-signal-strengthening-design.md`
- Modify: `docs/plans/2026-04-11-pm5d-v3-1-signal-strengthening-implementation-plan.md`

- [ ] Note the owned files for this slice and the verification target.
- [ ] Keep the scope narrow: strategy bundle only, no runner/runtime changes.

### Task 2: Add failing entry-gate tests first

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/strategies/directional.rs`

- [ ] Add a regression test where probability/edge are good but trailing trend
      persistence is too short, and assert no entry is produced.
- [ ] Run the targeted test and confirm it fails for the missing persistence
      gate.
- [ ] Add a regression test where probability/edge are good but signal-aligned
      consistency is too low, and assert no entry is produced.
- [ ] Run the targeted test and confirm it fails for the missing consistency
      gate.
- [ ] Add a control test where the signal remains strong, persistent, and
      consistent, and assert the entry still fires.

### Task 3: Implement the minimal strategy/config changes

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/strategies/directional.rs`
- Modify: `crates/ploy-strategy-bundles/src/strategies/mean_reversion.rs`
- Modify: `crates/ploy-strategy-bundles/examples/optimize_backtest.rs`
- Modify: `crates/ploy-strategy-bundles/examples/run_backtest.rs`
- Modify: `crates/ploy-strategy-bundles/tests/backtest_integration.rs`
- Modify: `config/strategies/02-pm5d.v3-dryrun.toml`
- Modify: `config/strategies/02-pm5d.v3-live.toml`

- [ ] Add `min_trend_consistency` and `min_trend_persistence_secs` to
      `DirectionalConfig` with compatibility defaults.
- [ ] Add the trailing-alignment helper(s) to `ReturnBuffer`.
- [ ] Enforce the new hard gates inside `evaluate_entry(...)`.
- [ ] Set only the V3 configs to the stronger thresholds; leave other variants
      on no-op defaults.
- [ ] Update all direct `DirectionalConfig { ... }` literals so the crate still
      compiles cleanly.

### Task 4: Verify behavior and summarize

**Files:**
- Modify: `tasks/todo.md`

- [ ] Run targeted tests for the directional strategy and confirm green output.
- [ ] Run a focused bundle-level check or test sweep for compile confidence.
- [ ] Run one local V2/V3 comparison or optimization/backtest check if the data
      path is available locally.
- [ ] Record what changed, what got stricter, and any remaining tuning risks.
