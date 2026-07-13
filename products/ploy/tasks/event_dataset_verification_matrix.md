# Event Dataset Verification Matrix

Owner: `worker-1`
Task: `8` verification lane for the event-id dataset slice

## Scope

This lane owns verification evidence for the event-aware dataset work under
`crates/ploy-research` and must not widen into optimize/backtest hot-path
changes.

## Shared-file conflict watchpoints

Treat these as shared/high-conflict paths and escalate before editing them from
this verification lane:

- `crates/ploy-research/src/lib.rs`
- `crates/ploy-research/src/factors.rs`
- `crates/ploy-research/examples/factor_research.rs`
- `crates/ploy-research/src/dataset/**` (expected new implementation area)
- `crates/ploy-research/Cargo.toml`

Hot-path files that must stay untouched by the event-dataset slice:

- `apps/ploy-backtest/**`
- `apps/ploy-replay/**`
- `crates/ploy-feed-loaders/**`
- `crates/ploy-strategy-bundles/**`
- `crates/ploy-strategy-runtime/**`
- `.github/workflows/optimize.yml`
- workspace `Cargo.toml`

## Verification matrix

| Check | Command | PASS condition |
| --- | --- | --- |
| Scope guard | `scripts/check_event_dataset_verification_lane.sh` | Only dataset-slice files changed; hot path untouched |
| Research crate typecheck | `cargo check -p ploy-research --tests` | exits 0 |
| Research crate unit tests | `cargo test -p ploy-research --lib` | exits 0 |
| Example compile guard | `cargo test -p ploy-research --example factor_research --features db,polars-export -- --nocapture` | exits 0 if dataset work touches example/export path |
| Dataset-focused test discovery | `cargo test -p ploy-research --lib -- --list | grep -Ei 'dataset|split|manifest|chronolog|event[_-]?summary'` | at least one dataset-oriented test name present once implementation lands |
| Targeted dataset tests | `cargo test -p ploy-research <matched-test-name> -- --nocapture` | exits 0 for each matched dataset/split/manifest test |
| Hot-path regression guard | `git diff --name-only <base>...HEAD -- apps/ploy-backtest apps/ploy-replay crates/ploy-feed-loaders crates/ploy-strategy-bundles crates/ploy-strategy-runtime .github/workflows/optimize.yml Cargo.toml` | no output |

## Required evidence mapped from approved test spec

1. **Leakage boundary**
   - prove no `event_id` appears in more than one split
   - prove split key is event-level, not row-level
2. **Chronology**
   - prove split anchor is `pm_market_metadata.end_time`
   - prove ordering tuple is exactly `(end_time, symbol, event_id)`
3. **Artifact contract**
   - show event index shape
   - show manifest shape
   - show split output shape
   - show event-summary output shape
4. **Task-grain correctness**
   - observation-level repricing label remains `future_up_ask_change_30s`
   - event-summary-level settlement label remains `settlement_up`
5. **Sequence/RL boundaries**
   - no second conflicting split system for sequence prep
   - RL remains deferred; no claim that current RL skeleton is execution-ready
6. **Hot-path protection**
   - optimize/backtest path remains unchanged

## Completion report shape

When implementation lands, report evidence in this exact order:

1. `PASS/FAIL Scope guard`
2. `PASS/FAIL cargo check -p ploy-research --tests`
3. `PASS/FAIL cargo test -p ploy-research --lib`
4. `PASS/FAIL dataset-focused tests`
5. `PASS/FAIL artifact evidence`
6. `PASS/FAIL hot-path regression guard`
7. Blockers / shared-file conflicts

## Notes

- Run the example compile guard only when implementation touches
  `examples/factor_research.rs` or the parquet/export surface.
- If dataset work adds new binary/example entrypoints, extend this matrix rather
  than replacing the existing checks.

## Verification run: current landed dataset slice

Verified slice: `16710042..59f7f775`
Comparison base for slice-scoped guards: `e5d39f89^`

### Structured evidence

1. `PASS Scope guard`
   - `scripts/check_event_dataset_verification_lane.sh e5d39f89^`
   - Output: `PASS: verification lane stayed within event dataset scope and left optimize/backtest hot paths untouched`
2. `PASS cargo check -p ploy-research --tests`
   - Command exited `0`
3. `PASS cargo test -p ploy-research --lib`
   - Output summary: `24 passed; 0 failed`
4. `PASS dataset-focused tests`
   - Discovery:
     - `cargo test -p ploy-research --lib -- --list | grep -Ei 'dataset|split|manifest|chronolog|event[_-]?summary'`
     - Matched 7 dataset tests under `dataset::chronology`, `dataset::contracts`, and `dataset::split`
   - Targeted run:
     - `cargo test -p ploy-research dataset:: --lib -- --nocapture`
     - Output summary: `7 passed; 0 failed`
   - Supporting task-grain evidence:
     - `cargo test -p ploy-research derived_artifacts_filter_to_selected_events_and_preserve_task_grains --lib -- --nocapture`
     - Output summary: `1 passed; 0 failed`
5. `PASS artifact evidence`
   - Chronology contract lives in `crates/ploy-research/src/dataset/chronology.rs`
   - Split contract + hard-failure policy live in `crates/ploy-research/src/dataset/split.rs`
   - Manifest / event-index contract lives in `crates/ploy-research/src/dataset/contracts.rs`
   - Event-summary / task-grain derived-view logic lives in `crates/ploy-research/src/factors.rs`
   - Crate exports are wired through `crates/ploy-research/src/dataset/mod.rs` and `crates/ploy-research/src/lib.rs`
6. `PASS hot-path regression guard`
   - `git diff --name-only e5d39f89^..HEAD -- apps/ploy-backtest apps/ploy-replay crates/ploy-feed-loaders crates/ploy-strategy-bundles crates/ploy-strategy-runtime .github/workflows/optimize.yml Cargo.toml`
   - Output: empty
7. `PASS modified-file lint`
   - `bash -n scripts/check_event_dataset_verification_lane.sh`
   - Output: empty

### Conditional check status

- `SKIP example compile guard`
  - `e5d39f89^..HEAD` did not touch `crates/ploy-research/examples/factor_research.rs`
    or the parquet/export surface, so the conditional example check was not required
    for this landed slice.

### Conflict watch result

- No new shared-file conflict required intervention from the verification lane.
- Verification evidence was recorded without editing dataset implementation files
  after they landed.
