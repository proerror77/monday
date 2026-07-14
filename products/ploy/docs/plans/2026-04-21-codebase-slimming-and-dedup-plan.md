# Ploy Codebase Slimming And Dedup Plan

Date: 2026-04-21

## Goal

Reduce local and CI iteration cost without changing trading behavior. The plan
focuses on dependency boundaries, duplicated runtime logic, and stale surfaces
that keep old paths expensive to compile or hard to reason about.

This is a planning artifact only. Implementation should happen in small,
behavior-locked slices.

## Quantitative Dependency Baseline (2026-04-21)

| Metric | Value |
|--------|-------|
| `new-ploy-runner` transitive deps | 1373 |
| `alloy` transitive deps (via SDK/claimer) | 736 (53% of runner total) |
| `alloy-*` sub-crates in runner tree | 135 |
| `polymarket-client-sdk` entry paths into runner | 3 (claimer, connectivity, market-data) |
| `sqlx` entry paths into runner | 4 (market-data, runner-host, strategy-bundles, strategy-runtime) |
| `sqlx` actual usage in strategy-bundles | 1 file (`feed/database.rs`) |
| `ploy-claimer` pulls | `alloy` + `ethers-core` + `ethers-signers` (two Ethereum stacks) |
| `ploy-strategy-bundles` transitive deps | 375 |
| Duplicated strategy state structs | 33+ across 10 files |
| `target/debug/` size | 2.5 GB |
| Total Rust target dirs across worktrees | ~17.6 GB |

## Current Evidence

- Phase 0 execution baseline lives in
  `docs/plans/2026-04-21-codebase-slimming-baseline.md`.
- Feature-matrix smoke commands live in `scripts/check_feature_matrix.sh`.
- The workspace has 18 Rust packages, but the default runner path still pulls
  multiple heavy operational crates through `new-ploy-runner -> ploy-runner-host
  -> ploy-strategy-runtime`.
- `Cargo.toml:24-38` keeps `new-ploy-runner`, `ploy-runner-host`, and
  platform crates in `default-members`.
- `crates/ploy-runner-host/Cargo.toml:10-17` depends directly on
  `ploy-market-data`, `ploy-strategy-bundles`, `ploy-strategy-runtime`, and
  `sqlx`.
- `crates/ploy-runner-host/src/lib.rs:1-5` imports collector, DB diagnostics,
  strategy config, runtime, and SQLx at the top level. Its CLI includes both
  strategy run and ops commands in one compilation unit.
- `crates/ploy-strategy-runtime/Cargo.toml:10-22` depends on live execution,
  claimer, market data, strategy bundles, SQLx, Tokio, and trading models.
- `crates/ploy-strategy-runtime/src/lib.rs:1-26` imports live feeds, scanner,
  sports feed, DB pool, claimer, live executor, simulated executor, and recorder
  into one file.
- `crates/ploy-strategy-runtime/src/lib.rs:87-178` has backtest/replay logic
  that does not need live feeds or live execution.
- `crates/ploy-strategy-runtime/src/lib.rs:180-310` wires dry-run/live feeds,
  DB polling, scanner, sports feed, recorder, live executor, and claimer.
- `crates/ploy-market-data/Cargo.toml:10-23` always pulls
  `polymarket-client-sdk`, `reqwest`, `sqlx`, `tokio-tungstenite`, and Tokio.
- `cargo tree -i polymarket-client-sdk` shows it enters `new-ploy-runner`
  through `ploy-claimer`, `ploy-connectivity`, and `ploy-market-data`.
- `cargo tree -i sqlx` shows SQLx enters `new-ploy-runner` through
  `ploy-market-data`, `ploy-runner-host`, `ploy-strategy-bundles`, and
  `ploy-strategy-runtime`.
- `crates/ploy-strategy-bundles/src/config.rs:35-45` hard-binds `FullConfig`
  to `DirectionalConfig`, while `config.rs:141-160` supports many strategy
  aliases and experimental variants.
- `crates/ploy-strategy-runtime/src/lib.rs:837-901` has a runtime-level strategy
  factory that only constructs five variants, while
  `crates/ploy-strategy-bundles/src/strategies/mod.rs:3-23` exports nine
  strategy modules.
- The PM5D strategy modules repeatedly define local `EventWindow`,
  `SpotState`, `QuoteState`, `HoldingState`, `candidate_events`, and
  settlement helpers across `directional`, `directional_bayes`, `mean_reversion`,
  `reversal`, `diff_*`, `prob_*`, `sweep`, and `three_layer`.
- `crates/ploy-market-contracts/src/events.rs:7-137` already provides the
  canonical `MarketUpdate`, and `crates/ploy-market-contracts/src/feed.rs:5-19`
  already provides the lightweight `Feed` trait. The old runtime-contract plan
  has therefore partially landed.
- `crates/ploy-strategy-bundles/src/feed/database.rs`, `feed/parquet.rs`, and
  `feed/parquet_stream.rs` each duplicate `MarketUpdate` timestamp ordering.
- `crates/ploy-research/Cargo.toml:10-31` pulls Polars, Linfa, Forust, Burn,
  SQLx, and strategy bundles in the same research crate.
- `crates/ploy-research/src/lib.rs:1-33` shows both old large modules
  (`factors`, `backtesting`) and newer layered modules (`factors_new`,
  `signal`, `backtest`, `attribution`, `model`) are exported together.
- `scripts/binance_*_collector.py`, `scripts/polymarket_quote_collector.py`,
  `scripts/check_db*.rs`, and `crates/ploy-market-data` overlap on live
  collection and DB diagnostics.
- `ploy-frontend/src/services/api.ts:1-220` and
  `ploy-sidecar/src/tools/ploy-backend.ts:14-67` duplicate control-plane DTO
  and auth/header knowledge instead of consuming a shared API contract.
- `.github/workflows/deploy-tango-1-1.yml:63-83` builds runner and research
  binaries together; `.github/workflows/deploy-tango-1-1.yml:65` runs
  `cargo clean -p new-ploy-runner`, which defeats incremental reuse in that
  job.

## Decision

Use a staged cleanup, not a big-bang rewrite.

1. First make dependency lanes explicit with features or small crates.
2. Then deduplicate strategy state and strategy factory wiring.
3. Then retire or quarantine duplicated ops/research/script surfaces.
4. Only after behavior is locked, patch vendored SDK dependency features.

### Deep Optimization Directions (Beyond Feature Gating)

Feature gating alone reduces compile cost but does not fix the structural
mismatch between the trading system's runtime modes and the code boundaries.
Three additional directions address this:

**Direction A: Split binaries by runtime mode.**

The four runtime modes (backtest, replay, dry-run, live) have fundamentally
different dependency needs. Instead of one binary with feature gates, build
separate binaries:

```
apps/ploy-replay      → strategy-bundles + trading only (target: ~200 deps)
apps/ploy-backtest    → + sqlx for DB loading (target: ~400 deps)
apps/new-ploy-runner  → full live/dry-run (1373 deps, unchanged)
```

`ploy-replay` is the likely highest-value target for strategy iteration:
strategy developers iterate on signal logic using recorded market-update files.
Today they pay for alloy, ethers, SDK, live feeds, and DB just to replay a
NDJSON file. A dedicated replay binary should be measured against the target of
compile feedback in seconds instead of minutes.

**Direction B: Move data loading out of strategy-bundles.**

`ploy-strategy-bundles` should be pure strategy logic (signal → intent) with
zero IO dependencies. Currently `sqlx` leaks in through `feed/database.rs` —
a data loader that belongs in the runtime or a dedicated `ploy-feed-loaders`
crate. Moving it out makes the strategy crate a pure computation library:
fast to compile, trivial to test without IO mocks.

**Direction C: Consolidate claimer's dual Ethereum stacks.**

`ploy-claimer` currently pulls both `alloy` (new) and `ethers-core` +
`ethers-signers` (deprecated). Analysis shows:

- `alloy` is used for: on-chain contract calls (`sol!` bindings for
  `IConditionalTokens`, `INegRiskAdapter`), wallet/signer, provider, and
  transaction building.
- `ethers-core` + `ethers-signers` are used only in the relayer legacy flow
  (`relayer/legacy_flow.rs`, `relayer/proxy_support.rs`) for EIP-712 signing,
  ABI encoding, and address conversion.
- The vendored SDK's `ctf` module already provides `redeem_positions()` and
  `redeem_neg_risk()` via alloy — functionally overlapping with claimer's own
  `sol!` contract bindings.

Two consolidation paths:

1. **Migrate relayer legacy flow from ethers to alloy** — removes `ethers-core`
   + `ethers-signers` entirely. The bridge functions (`ethers_to_alloy_address`)
   confirm this is a mechanical migration.
2. **Delegate on-chain redeem to SDK's CTF client** — removes claimer's own
   `sol!` contract bindings and reduces claimer to: discovery (Data API) +
   claim orchestration (relayer or SDK CTF). This is a larger change but
   eliminates the contract-binding duplication.

Note: Polymarket V2 may change the claim/redeem flow. If V2's relayer or
protocol handles auto-redemption natively, `ploy-claimer` may become
unnecessary entirely. This should be verified after V2 migration stabilizes
(April 28, 2026 cutover).

## Non-Goals

- No live-trading behavior change in the first cleanup phase.
- No strategy threshold or alpha tuning.
- No local full workspace release builds as proof; use targeted checks locally
  and CI/remote for heavy Rust/DuckDB/Polars verification.
- No new external dependencies unless a later implementation slice explicitly
  justifies them.

## Rollback Strategy

Each phase should be safely revertible. General rule:

- Cargo.toml feature additions are additive — revert = remove the feature flags
  and restore unconditional deps.
- Strategy common-state extraction (Phase 3) has a point of no return once
  multiple strategies consume the shared module. Migrate one strategy first and
  validate before batch-migrating the rest.
- CI workflow changes (Phase 8) can be reverted by restoring the previous
  workflow YAML.
- If a phase is half-landed and broken, revert the entire phase rather than
  patching forward.

## Phase 0 - Baseline And Guardrails

### Tasks

- Capture current dependency evidence:
  - `cargo tree -p new-ploy-runner --edges normal --depth 2`
  - `cargo tree --workspace --edges normal -i polymarket-client-sdk`
  - `cargo tree --workspace --edges normal -i sqlx`
  - `cargo tree -p ploy-strategy-bundles --features parquet-feed -i duckdb`
  - `cargo tree --workspace --edges normal -i polars`
- Add or identify existing behavior tests for:
  - runtime mode dispatch
  - strategy alias resolution
  - live/dry-run DB failure policy
  - replay/backtest config parsing
  - strategy entry/exit invariants for PM5D variants
- Add a lightweight warning inventory for unused strategy fields/methods, but
  do not suppress warnings unless a field is intentionally preserved for
  serialization or forward compatibility.
- Fix the `Regime` re-export conflict in `ploy-research/src/lib.rs` — it
  currently re-exports `Regime` from `ploy_operator_contracts` while
  `factors_new` exports its own `Regime`. Resolve the ambiguity before deeper
  research cleanup.
- Add a feature-matrix smoke script (`scripts/check_feature_matrix.sh` or
  Makefile target) that runs `cargo check` with each meaningful feature
  combination. This prevents silent feature-unification bugs in later phases.

### Acceptance

- A baseline report records dependency fan-in for SDK, SQLx, DuckDB, Polars,
  Burn/Linfa/Forust, and optional features.
- Each listed behavior (runtime mode dispatch, strategy alias resolution,
  live/dry-run DB failure policy, replay/backtest config parsing, PM5D
  entry/exit invariants) has at least one test that would fail if the behavior
  changed. Test names are recorded in the baseline report.
- The `Regime` re-export ambiguity is resolved.
- A feature-matrix smoke script exists, `--list` passes, and targeted local
  cargo checks pass. The full matrix runs in CI or an isolated target.
- Any skipped heavy validation is explicitly documented with the remote/CI path
  that will cover it.

## Phase 0.5 - Quick Dependency Slimming (Highest ROI)

### Problem

`alloy` alone contributes 736 transitive dependencies (53% of the runner's
total). It enters through `ploy-claimer` and `ploy-connectivity`, both of which
are unconditional dependencies of `ploy-strategy-runtime`. But backtest, replay,
and dry-run modes never use the claimer or live execution. Similarly, `sqlx` in
`ploy-strategy-bundles` is unconditional but only used in one file.

This phase targets the four likely highest-ROI Cargo.toml changes. The current
dependency baseline suggests they can cut a large share of explicit
lean/no-default development builds while the default runner remains full. The
actual reduction must be measured after the feature split lands.

### Plan

Four changes, ordered by impact:

1. **`ploy-strategy-runtime`**: make `ploy-claimer` optional, gate behind
   `auto-claimer` feature.
   - Guard `use ploy_claimer::ensure_account_claimer_daemon` and its call site
     with `#[cfg(feature = "auto-claimer")]`.
   - Impact: removes `alloy` (736 deps) + `ethers-core` + `ethers-signers` from
     lean/no-default runtime builds while preserving the default full runner.

2. **`ploy-strategy-runtime`**: make `ploy-connectivity` optional, gate behind
   `live-execution` feature.
   - Guard live executor imports and the live/dry-run execution wiring with
     `#[cfg(feature = "live-execution")]`.
   - Impact: removes `polymarket-client-sdk`'s second entry path from
     lean/no-default runtime builds.

3. **`ploy-strategy-bundles`**: make `sqlx` optional, gate behind `db-feed`
   feature.
   - Only `feed/database.rs` uses `sqlx`. Wrap the module with
     `#[cfg(feature = "db-feed")]` and re-export conditionally.
   - Impact: removes `sqlx` from strategy-bundles lean/no-default builds.
   - Longer term (Phase 2): move `feed/database.rs` out of strategy-bundles
     entirely into `ploy-strategy-runtime` or a dedicated `ploy-feed-loaders`
     crate. Strategy-bundles should be pure computation (signal → intent) with
     zero IO dependencies. This makes it trivial to test without DB mocks and
     fast to compile.

4. **`ploy-market-data`**: make `polymarket-client-sdk` optional, gate behind
   `live` feature.
   - Guard SDK imports, `reqwest`, `tokio-tungstenite`, and live collector/
     scanner modules behind `#[cfg(feature = "live")]`.
   - Keep `ploy-market-contracts` as the unconditional lightweight dependency.
   - Impact: removes SDK + reqwest + WebSocket from market-data lean/no-default
     builds.

These four changes must land with a runner feature spine in the same atomic
slice:

- `new-ploy-runner` default/full build keeps the current live/dry-run/ops
  behavior and enables the full runner host feature set.
- Lean replay/backtest builds must be separate Cargo feature builds or separate
  binary entrypoints. Do not rely on a runtime subcommand of one already-built
  binary to reduce compiled dependencies.
- `ploy-runner-host` forwards explicit full vs lean features to
  `ploy-strategy-runtime`, `ploy-market-data`, and `ploy-strategy-bundles`.
- Ops commands that need live discovery or DB access stay on the full/ops
  build surface.

### Rollback

All four changes are purely additive Cargo.toml feature flags + `cfg` guards.
Revert = remove the feature declarations and restore unconditional deps. No
struct/function signatures change.

### Acceptance

- `cargo tree -p ploy-strategy-runtime --no-default-features -i alloy` returns
  empty (alloy no longer in the lean/no-default runtime path).
- `cargo tree -p ploy-strategy-runtime --no-default-features -i ploy-claimer`
  returns empty.
- `cargo tree -p ploy-strategy-runtime --no-default-features -i ploy-connectivity`
  returns empty.
- `cargo tree -p ploy-strategy-bundles --no-default-features -i sqlx` returns
  empty.
- `cargo tree -p ploy-market-data --no-default-features -i polymarket-client-sdk`
  returns empty.
- `cargo check -p new-ploy-runner` still passes (with appropriate feature flags
  for the target mode).
- `cargo test -p ploy-strategy-runtime --lib` still passes.
- `cargo test -p ploy-strategy-bundles --lib` still passes.
- Full-feature build (`--all-features` or explicit live feature set) still
  compiles and passes tests.

## Phase 1 - Split Runner And Market-Data Compile Lanes

### Problem

`new-ploy-runner` currently compiles runner, live feeds, DB diagnostics, quote
collector, scanner, sports capture, live execution, and claimer together. This
makes ordinary backtest/replay/dry-run code pay for dependencies it does not
always need.

### Plan

- Split `ploy-runner-host` into two command ownership surfaces:
  - `run` path: strategy config parsing and runtime launch only.
  - ops path: `check-db`, `collect-quotes`, and future data tooling.
- Prefer one of these shapes:
  - `apps/ploy-datactl` + `crates/ploy-datactl-host` for ops commands, or
  - feature-gated `ploy-runner-host/ops` module if a new binary is too much for
    the first slice.
- Move direct imports of `QuoteCollector`, `check_database`, and `PgPoolOptions`
  out of the default runner module.
- Split `ploy-market-data` dependency features in two passes:
  - First pass (this phase): coarse split into 2-3 features:
    - `default`: contracts + lightweight types only (no SDK, no SQLx, no WS).
    - `live`: full current behavior (SDK, SQLx, WebSocket, scanner, sports).
    - Optionally `store-postgres` if the DB boundary is clean enough.
  - Second pass (follow-up after Phase 1 proves the boundary): refine `live`
    into finer features (`gamma`, `clob-ws`, `rtds`, `binance-ws`, `sports-ws`)
    only if the coarse split is stable and the finer split has a concrete
    consumer.
  - Rationale: Cargo feature unification bugs are hard to debug. Starting
    coarse and refining is safer than starting with 7 features.
- Keep `ploy-market-contracts` as the canonical event/Feed crate.

### Acceptance

- `cargo tree -p new-ploy-runner --edges normal -i polymarket-client-sdk` no
  longer includes the SDK for backtest/replay-only builds.
- `cargo tree -p new-ploy-runner --edges normal -i sqlx` only appears when DB
  backtest, recorder, or live persistence features are enabled.
- `new-ploy-runner run --config ...` behavior remains unchanged under the
  existing default feature set.
- `check-db` and `collect-quotes` remain available through the selected ops
  binary/feature.

## Phase 2 - Split Strategy Runtime Into Backtest, Replay, Dry-Run, Live

### Problem

`ploy-strategy-runtime` mixes:

- backtest DB loading
- replay NDJSON loading
- live/dry-run feed fanout
- signal/order/fill persistence
- live Polymarket execution
- claimer startup

### Plan

- Extract mode modules:
  - `modes/backtest.rs`
  - `modes/replay.rs`
  - `modes/live.rs`
  - `recording.rs`
  - `strategy_factory.rs`
- Make live execution dependencies optional:
  - `live-execution` gates `ploy-connectivity`.
  - `auto-claimer` gates `ploy-claimer`.
  - `live-feeds` gates live `ploy-market-data` feed fanout.
  - `db-recorder` gates SQLx recorder.
- Keep the public `run_strategy(config, path, force_dry_run)` facade so CLI
  behavior does not churn.
- Move `RuntimeDbRecorder` to its own module or crate so SQL persistence can be
  compiled independently from pure replay/backtest.
- Move `feed/database.rs` from `ploy-strategy-bundles` into
  `ploy-strategy-runtime` (or a dedicated `ploy-feed-loaders` crate) so that
  strategy-bundles becomes a pure computation library with zero IO deps.

#### Split binaries by runtime mode (Direction A)

After the mode modules are extracted, introduce lightweight binary entrypoints
that only link the dependencies their mode actually needs:

```
apps/ploy-replay      → ploy-strategy-bundles + ploy-trading
                         (target: ~200 deps; compile-time target: seconds)
                         Reads NDJSON file, runs strategy, prints results.

apps/ploy-backtest    → ploy-replay deps + sqlx (for DB loading)
                         (~400 deps)
                         Reads from database, runs strategy, prints results.

apps/new-ploy-runner  → full live/dry-run (unchanged, 1373 deps)
                         Live feeds, execution, claimer, DB recording.
```

`ploy-replay` is the likely highest-value target for strategy development
iteration: change signal logic → `cargo run -p ploy-replay -- --config ...` →
measure whether feedback drops from minutes to seconds. Today this path
compiles alloy, ethers, SDK, live feeds, and DB for no reason.

Each binary reuses the extracted mode modules from `ploy-strategy-runtime` —
the split is at the binary/Cargo.toml level, not code duplication.

### Acceptance

- Backtest/replay module checks do not compile `ploy-connectivity` or
  `ploy-claimer`.
- `ploy-replay` binary compiles without `alloy`, `ethers`, `sqlx`, or
  `polymarket-client-sdk` in its dependency tree.
- `ploy-backtest` binary compiles without `alloy`, `ethers`, or
  `polymarket-client-sdk`.
- Live/dry-run checks still compile and retain current DB fatality policy.
- Existing tests for `database_unavailable_is_fatal`, strategy alias dispatch,
  and replay/backtest parsing still pass.
- `ploy-strategy-bundles` has zero IO dependencies (no `sqlx`, no `duckdb` in
  default features).

## Phase 3 - Deduplicate PM5D Strategy State And Helpers (Isolated Worktree Only)

### Problem

Every strategy variant carries similar local event, quote, spot, holding,
candidate, and settlement code. This creates unused-field warnings, inconsistent
bug fixes, and high review cost.

Note: This phase is entirely within `ploy-strategy-bundles/src/strategies/` and
does not touch dependency boundaries, `Cargo.toml`, `feed/*`, `config.rs`, or
runtime factory code. It can run in parallel with dependency-boundary work only
from a separate worktree with explicit file ownership. In the current dirty
worktree, classify existing strategy edits before starting this phase.

### Plan

- Add `crates/ploy-strategy-bundles/src/strategies/common/`:
  - `event.rs`: `EventWindow`, token/event lookup, candidate sorting.
  - `quote.rs`: `QuoteState`, freshness helpers, best-side accessors.
  - `spot.rs`: spot history/current price helpers.
  - `settlement.rs`: resolved outcome logic.
  - `guards.rs`: open-position / active-order duplicate prevention.
  - `holding.rs`: minimal holding state shared by exit-capable strategies.
- Migrate one strategy first, preferably `reversal` or `mean_reversion`, because
  they are smaller than `directional` but exercise event/quote/depth paths.
- Migrate `directional` and `directional_bayes` second, keeping Bayesian logic
  separate but sharing common event/quote/guard code.
- Migrate `diff_*`, `prob_*`, and `sweep` after the common helpers prove stable.
- Delete local state fields that are not used by behavior. Preserve a field only
  when a test or runtime path consumes it.

### Acceptance

- `rg '^struct (EventWindow|SpotState|QuoteState|HoldingState)' crates/ploy-strategy-bundles/src/strategies`
  returns only the common module definitions plus deliberate special cases.
- The unused warning set for PM5D strategy state shrinks without broad
  `#[allow(dead_code)]` additions.
- Strategy-specific tests pass after each migration.
- `cargo test -p ploy-strategy-bundles <strategy-name> --lib` is the default
  local verification for each slice.

## Phase 4 - Replace DirectionalConfig-As-Universal-Config

### Problem

`FullConfig.strategy` is `DirectionalConfig`, then runtime converts or reuses it
for unrelated strategies. This made sense for compatibility, but it now hides
strategy ownership and makes config defaults leak across variants.

### Plan

- Introduce a typed strategy registry:
  - `StrategyKind`
  - `StrategyFactory`
  - `StrategyConfigEnvelope`
- Move alias normalization out of `RuntimeSection` into the registry.
- Let each strategy own its config:
  - `DirectionalConfig`
  - `BayesianDirectionalConfig`
  - `MeanReversionConfig`
  - `ReversalConfig`
  - `ThreeLayerConfig`
  - future experimental strategy configs
- Keep TOML compatibility by supporting the existing `[strategy]` surface during
  a transition. Parse into the typed config selected by `[runtime].strategy_variant`.
- Add config tests for every checked-in `config/strategies/*.toml`.

### Acceptance

- All checked-in strategy TOMLs parse through the registry.
- Adding a new strategy no longer requires editing a central `match` in
  `ploy-strategy-runtime/src/lib.rs`.
- Strategy-specific defaults are tested in their strategy modules, not only in
  the global config parser.

## Phase 5 - Research Crate Cleanup And Feature Gating

### Problem

`ploy-research` has both old large modules and new layered modules. It also
pulls Polars, Linfa, Forust, Burn, SQLx, and strategy bundles together.

### Plan

- Turn research dependencies into explicit features:
  - `default = []` or a small default suitable for type checks.
  - `db` for SQLx loaders.
  - `polars-export` for Polars DataFrame/Parquet.
  - `ml` for Linfa/Forust.
  - `rl` for Burn.
  - `strategy-runtime` only where the research code really needs full strategy
    bundle execution.
- Also address `ploy-strategy-bundles` DuckDB/Parquet dependency:
  - The existing `parquet-feed` feature in strategy-bundles pulls DuckDB. Verify
    this feature is properly gated and not leaking into lean/no-default builds.
  - If DuckDB enters the runner through strategy-bundles, gate it behind an
    explicit feature in this phase.
- Finish the layered migration:
  - keep `factors.rs` as a compatibility facade temporarily.
  - move factor observation model, loaders, metrics, export, and IC reporting
    into focused modules.
  - shrink `examples/factor_research.rs` into CLI parsing plus orchestration.
- Fix the public export mismatch before deeper work:
  - `crates/ploy-research/src/lib.rs` currently re-exports `Regime` from
    `ploy_operator_contracts` while `factors_new/mod.rs` exports its own
    `Regime`.
- Keep heavy research validation on remote DB/CI by default.

### Acceptance

- `cargo tree -p ploy-research --edges normal -i polars` appears only with the
  Polars feature.
- `cargo tree -p ploy-research --edges normal -i burn` appears only with the RL
  feature.
- `factor_research` still builds under the feature set documented for it.
- Public exports no longer expose two incompatible `Regime` concepts.

## Phase 6 - Ops Scripts And Data Jobs Inventory

### Problem

There are Python live collectors, Rust market-data collectors, historical
backfill scripts, DB checks, and workflow jobs that overlap in purpose.

### Plan

- Create `docs/operations/data-jobs-inventory.md` with each script/job marked:
  - canonical
  - compatibility
  - one-shot backfill
  - retired/archive candidate
- Move live-collector functionality toward Rust market-data or a dedicated data
  ops binary.
- Keep Python only for one-shot repair/backfill/export jobs where it is still
  clearly useful.
- Archive or remove duplicate `check_db*.rs` scripts after `check-db` has a
  canonical home.
- Ensure all docs point to one collector runbook and one deployment path.

### Acceptance

- Every non-archived script has an owner, runtime context, and replacement
  status.
- No active runbook points to a retired single-binary or direct live path.
- Duplicate collector scripts are either retired or documented as compatibility
  only.

## Phase 7 - Control-Plane API Contract Cleanup

### Problem

Rust DTOs, frontend TypeScript types, and sidecar TypeScript response types are
kept in sync manually.

### Plan

- Treat `ploy-operator-contracts` as the source of truth.
- Use `schemars` to derive JSON Schema from Rust types and check the schema
  into the repo (e.g. `contracts/schemas/*.json`).
- Add a generated or checked schema snapshot for:
  - deployment summary/record
  - trading state snapshot
  - system status
  - audit events
  - control requests
- Make `ploy-frontend` and `ploy-sidecar` consume that contract rather than
  copying partial DTOs. Frontend/sidecar CI validates TypeScript types against
  the checked-in JSON Schema.
- Keep auth/header handling in one frontend API helper and one sidecar client
  helper, with tests for token precedence.

### Acceptance

- A DTO field added in Rust fails frontend/sidecar contract checks if not
  reflected.
- Frontend and sidecar no longer duplicate incompatible deployment response
  shapes.

## Phase 8 - CI And Build-Speed Work

### Problem

Some workflows already use cache, but deployment/research jobs still build
large lanes together and one deploy workflow explicitly cleans the runner
package before building.

### Plan

- Remove unnecessary `cargo clean -p new-ploy-runner` from deploy workflows.
- Split CI jobs by dependency lane:
  - control-plane/core
  - runner default
  - runner live features
  - market-data ops
  - research heavy features
  - frontend/sidecar
- Keep `sccache` and Rust cache, but measure cache hit rate and wall clock in
  job summaries.
- Prefer artifact build once, deploy many. Do not build Rust on live trading
  hosts.

### Acceptance

- CI reports per-lane build time and cache stats.
- `new-ploy-runner` fast lane no longer builds research/ops-only binaries.
- Production deploy still uses CI-built artifacts.

## Phase 9 - Vendored SDK Dependency Slimming

### Problem

The vendored Polymarket SDK unconditionally depends on `alloy` and `reqwest`.
This limits how much `ploy-market-data` can slim down if it uses SDK DTOs for
public discovery only.

### Plan

- Defer this until after the Polymarket V2 migration is stable and a V2
  claim/redeem evidence report exists. This is a blocking gate, not an
  implementation preference.
- Audit SDK modules by feature:
  - `gamma`
  - `data`
  - `clob`
  - `ctf`
  - `rtds`
  - `ws`
  - `heartbeats`
- Make `alloy` optional if only signing/order/CTF paths need it.
- Keep public Gamma DTO paths usable without signing dependencies, or replace
  Ploy's public discovery path with local DTOs plus `reqwest` if that is lower
  risk.

### Acceptance

- `cargo tree -p polymarket-client-sdk --no-default-features --features gamma`
  does not include `alloy` unless the SDK truly requires it.
- Downstream Ploy crates compile with narrower SDK features.

## Phase 10 - Claimer Consolidation Or Retirement (Post-V2)

### Problem

`ploy-claimer` has three structural issues:

1. **Dual Ethereum stacks**: pulls both `alloy` (new) and `ethers-core` +
   `ethers-signers` (deprecated). `ethers` is only used in the relayer legacy
   flow (`relayer/legacy_flow.rs`, `relayer/proxy_support.rs`) for EIP-712
   signing, ABI encoding, and address conversion. Bridge functions like
   `ethers_to_alloy_address` confirm this is a mechanical migration target.

2. **Contract binding duplication**: claimer defines its own `sol!` bindings for
   `IConditionalTokens.redeemPositions` and `INegRiskAdapter.redeemPositions`.
   The vendored SDK's `ctf` module already provides `redeem_positions()` and
   `redeem_neg_risk()` via alloy — functionally identical.

3. **V2 may make self-claim unnecessary**: Polymarket V2 (cutover April 28,
   2026) may change the claim/redeem flow. If V2's relayer or protocol handles
   auto-redemption natively, `ploy-claimer` may become unnecessary entirely.
   The `builder_relayer_sdk` feature already points toward a gasless relayer
   path that could replace on-chain claiming.

### Plan

Operator update: `ploy-claimer` has been retired after account settlement flow
conversion. The retained plan below is historical context for the decision.

After V2 migration stabilizes:

1. **Verify V2 claim behavior**: check whether V2 auto-redeems winning
   positions or still requires manual `redeemPositions` calls. Only if V2
   auto-redeems or provides an equivalent relayer path should step 4 proceed.

2. **Migrate relayer legacy flow from ethers to alloy**: replace
   `ethers_core::types`, `ethers_signers::LocalWallet`, and ABI encoding with
   alloy equivalents. Remove `ethers-core` and `ethers-signers` from
   `Cargo.toml`. This is a mechanical migration — the bridge functions already
   map the types.

3. **Delegate on-chain redeem to SDK CTF client**: replace claimer's own `sol!`
   contract bindings with calls to `polymarket-client-sdk::ctf::CtfClient`'s
   `redeem_positions()` and `redeem_neg_risk()`. Claimer becomes: discovery
   (Data API) + claim orchestration (relayer or SDK CTF) — no direct contract
   interaction.

4. **If V2 auto-redeems or equivalent behavior is verified**: treat
   `ploy-claimer` as a retirement candidate. Only after live claim/redeem
   behavior is proven should the crate, the `auto-claimer` feature from
   `ploy-strategy-runtime`, and the `ensure_account_claimer_daemon` call be
   removed.

### Acceptance

- If claimer is retained: `cargo tree -p ploy-claimer -i ethers-core` returns
  empty (ethers fully removed).
- If claimer is retained: claimer no longer defines its own `sol!` contract
  bindings for `IConditionalTokens` or `INegRiskAdapter`.
- If claimer is retired: `ploy-strategy-runtime` compiles without
  `ploy-claimer` in any feature configuration.
- Workspace metadata no longer includes `ploy-claimer`.

## Verification Matrix

Use targeted local checks first:

- `cargo check -p ploy-strategy-runtime --no-default-features` (Phase 0.5)
- `cargo check -p ploy-strategy-bundles --no-default-features` (Phase 0.5)
- `cargo check -p ploy-market-data --no-default-features` (Phase 0.5)
- `cargo tree -p ploy-strategy-runtime --no-default-features -i alloy` (Phase 0.5, expect empty)
- `cargo check -p ploy-replay` (Phase 2, expect no alloy/ethers/sqlx/SDK)
- `cargo check -p ploy-backtest` (Phase 2, expect no alloy/ethers/SDK)
- `cargo test -p ploy-strategy-bundles <strategy> --lib`
- `cargo test -p ploy-strategy-runtime --lib`
- `cargo check -p new-ploy-runner`
- `cargo check -p ploy-market-data --no-default-features`
- `cargo check -p ploy-market-data --features live`
- `cargo check -p ploy-research --no-default-features`
- `cargo check -p ploy-research --features db,polars-export`
- `cargo tree -p ploy-claimer -i ethers-core` (Phase 10, expect empty if retained)

Use CI/remote for heavy checks:

- full workspace check
- research Polars/Burn builds
- DuckDB/Parquet path
- database-backed backtest smoke
- live/dry-run remote acceptance

## Implementation Order

1. Phase 0 baseline, tests, Regime fix, and feature-matrix smoke check.
2. Phase 0.5 feature spine + runner forwarding (targets alloy/ethers/SDK/sqlx
   with explicit full vs lean Cargo build surfaces).
3. Phase 1 runner/market-data compile lane split.
4. Phase 2 runtime mode split + binary-per-mode (ploy-replay, ploy-backtest).
5. Phase 3 PM5D strategy common-state extraction, only in a separate worktree
   and only under `crates/ploy-strategy-bundles/src/strategies/**`.
6. Phase 4 typed strategy registry/config (after Phase 3).
7. Phase 5 research feature gating and cleanup (after Phase 2 proves runtime
   and feed boundaries). Before Phase 2, only research dependency inventory and
   feature design are allowed.
8. Phase 6 ops script inventory and retirements.
9. Phase 7 API contract cleanup.
10. Phase 8 CI build-speed work.
11. Phase 9 SDK slimming, blocked on V2 migration stability plus claim/redeem
    evidence.
12. Phase 10 claimer consolidation or candidate retirement investigation,
    blocked on V2 claim/redeem evidence (after V2 stabilizes, ~May 2026).

Phase 0.5 is the current highest-ROI hypothesis: four Cargo.toml changes target
the alloy/ethers-heavy paths for explicit lean/no-default builds while the
default runner remains full. Measure the actual dependency and compile-time
reduction after it lands.

Phase 2's binary split is the largest current strategy-iteration hypothesis:
target `ploy-replay` around ~200 deps versus the current 1373-line runner tree,
then measure whether signal-logic feedback drops from minutes to seconds.

Phase 10 depends on V2 migration outcome. Treat `ploy-claimer` retirement as a
candidate only after V2 claim/redeem behavior proves auto-redemption or an
equivalent relayer path.
