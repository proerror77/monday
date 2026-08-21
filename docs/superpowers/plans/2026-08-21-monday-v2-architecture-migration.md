# Monday V2 architecture migration plan

**Status:** Phase 1 runtime-admission extraction complete; attribution and bundle
extraction pending on
`codex/monday-flow-fix-snapshot@5ec72cc8`

**Owner:** current checkout only; no production controller is assigned

**Scope:** repository architecture and migration seams; no runtime cutover

This plan implements [ADR-0001](../../architecture/ADR-0001-monday-v2-system-boundaries.md)
incrementally. The first slice was documentation/evidence; the runtime-admission
extraction is the first bounded code slice. They must not compete with the
existing Collector recovery work or the user's dirty files.

## Current baseline

| Area | Current evidence | Interpretation |
| --- | --- | --- |
| Checkout | `codex/monday-flow-fix-snapshot`, `5ec72cc8` | Not `main`; recheck before every publish/merge action |
| Worktree | 25 modified tracked files; untracked `DEV_STATE.md`, ADR/inventory/plan documents, `rust_hft/.cargo_home/git/`, and `rust_hft/governance-contracts/` | Includes pre-existing user changes; preserve, no reset, cleanup, or broad formatting |
| Workspace graph | 78 packages in `rust_hft`; 23 in `rust_hft/prediction-markets` | The new governance package is a contract seam, not a new product authority |
| Data | `hft-collector` has 50 Rust files and recorder/reference/uploader/materializer/cache binaries | Collection and heavy processing share a package/release boundary |
| Research | `alpha-harness` splits CLI files but links collector and backtest directly; multiple mission/loop seams remain | Operator acceptance and implementation paths are not yet one Golden Path |
| Contracts | `alpha-domain` is 7,445 lines; `hft-governance-contracts` is 511 lines and `hft-live` now consumes it directly | Runtime admission is separated; attribution and bundle identity remain shared |
| Prediction | `ploy-research` can opt into `ploy-strategy-bundles` and `ploy-trading` | Transitional runtime coupling must shrink, not gain authority |
| Safety | `cargo metadata --locked --no-deps` succeeds; no direct alpha/research -> risk/execution edge observed | Safe to start with docs and contract-boundary work |

## Phases

### Phase 0 — establish the contract (complete)

- Add ADR-0001 and this plan.
- Update the repository entry point so Monday is described as the full governed
  research/trading system.
- Record current branch, dirty files, package counts, dependency findings, and
  stop rules.

**Check:** `git diff --check`; `cargo metadata --locked --no-deps`; no runtime,
deployment, or data artifacts are changed.

### Phase 1 — extract governance contracts

**Target:** create the smallest functional governance-contract crate and move only
the shared runtime-admission types required by `apps/live` first. Strategy Bundle
identity, promotion references, and signed runtime attribution remain separate
follow-up slices.

**Inventory slice (complete):**
[governance-contract-inventory.md](../../architecture/governance-contract-inventory.md)
names the exact source spans, consumers, regression vectors, ownership blockers,
and extraction stop rules. The runtime-admission family now lives in
`hft-governance-contracts`; the store and alpha CLI consume it directly.

**Order:**

1. ~~Inventory type ownership and schema/hash tests in `alpha-domain`~~
   (complete).
2. ~~Move runtime admission and its canonical hash/signature helpers while
   preserving serialization and hash vectors~~ (complete).
3. ~~Migrate the research store and alpha CLI from the temporary re-export~~
   (complete); the old runtime-admission export is deleted.

**Checks:** package-scoped tests for `alpha-domain`, the new contract crate, and
`hft-live`; exact JSON/hash regression vectors; `cargo metadata --locked --no-deps`.

**Stop:** any V1 identity drift, signed-envelope mismatch, or runtime behavior
change. Do not combine this phase with Strategy/OrderIntent changes.

### Phase 2 — separate data-plane release boundaries

Keep the current public collectors working while splitting responsibilities by
owner and workload:

1. Edge capture: WebSocket/REST acquisition, bounded queues, durable raw segment
   append, rotation, and raw-byte shipper.
2. Verification/reference: independent manifest, clock, sequence, completeness,
   and reference artifact checks.
3. Heavy materialization: canonical Parquet, PIT snapshots, replay partitions,
   cache warming, and optional ClickHouse analytics.

Start with one vertical slice (Binance LOB or Polymarket raw upload), not a
repository-wide package rename. The old binary remains the rollback path until the
new unit has separate artifact identity and independent readback.

**Checks:** recorder memory/queue behavior, manifest/hash parity, `_SUCCESS`,
OSS readback, and the nearest collector contract tests. A production Gate is only
required when the new unit crosses a runtime/collector cutover.

### Phase 3 — make strategy plans explicit

Define a content-addressed compiled strategy plan that can be interpreted by
research, replay, backtest, Paper, and Shadow. Add `TargetExposure` and a runtime
allocator/order-planner seam behind the existing strategy adapter.

The first implementation must use one Formula strategy and one instrument. Prove
signal, sizing, risk, and execution parity before broadening to ensembles,
multi-symbol, or multi-venue allocation. Runtime remains the sole order authority.

**Checks:** batch/online/checkpoint parity, replay-to-Paper parity, risk clamp
regression, and existing deployment-envelope tests.

### Phase 4 — make one research Golden Path

Declare the current operator acceptance seam (`mission dispatch submit`) as the
official CEX path. Keep lower-level commands for diagnostics only. Connect the
typed planner output to the existing deterministic validator/evaluator, then run
one exact-main mission with immutable result publication and independent readback.

Research parameter changes (symbol, horizon, factor set, budget, seed, cohort) must
remain Mission artifacts. Only missing capability, evaluator semantics, or runtime
contract opens an engineering change.

**Checks:** fresh source revision, mission/materialization/result hashes, terminal
status, and readback. Do not treat a merged PR, image, or submitted Job as
completion.

### Phase 5 — reduce prediction-market compatibility debt

Keep the nested workspace as a build seam while moving event-settlement data,
research, and operator views toward canonical Monday interfaces. Remove the
`ploy-research` strategy-runtime feature coupling after the backtest/replay DTOs
have independent ownership. Migrate venue adapters before any live integration;
delete superseded compatibility paths only after their readback condition is met.

**Checks:** nested workspace tests, no new `ploy-*` crates, no new order/risk/
reconciliation authority, and a dependency scan proving research no longer needs
runtime lifecycle types for its default path.

### Phase 6 — consolidate operations and evidence control

Keep one named controller per deployment target. Record source revision, binary
artifact hash, bundle hash, Gate verdict, cutover marker, service/timer state, and
data/OSS readback separately. Decouple uploader-only releases from recorder Gates
after the current incident is closed and the replacement is independently proven.

**Checks:** formal Gate only at the protected transition; direct runtime and OSS
readback; rollback identity and cleanup evidence.

## Explicit non-goals for this plan

- No new LLM provider, MCTS algorithm, model trainer, or microservice.
- No deletion of current collector, prediction, or runtime paths in Phase 0.
- No production deployment, credentials, live orders, Gate/cutover, or LiveSmall
  activation.
- No broad root-level `common`, `shared`, or exchange-branded tree.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Contract extraction changes historical hashes | Freeze V1 vectors and migrate one family at a time |
| Collector split duplicates upload/verification logic | One owner per invariant; keep the old path until independent readback |
| Strategy-plan work changes order semantics accidentally | Adapter first; require replay/Paper/Shadow parity before removal |
| Prediction compatibility code gains authority | Keep nested workspace transitional and fail closed; no new `ploy-*` authority |
| Dirty checkout hides overlap | Re-read branch/status and stop on movement before code edits |
| Gate becomes an exploration loop | Run targeted checks during development; formal Gate only at the boundary it protects |

## Definition of done for the next bounded slice

The runtime-admission slice is complete: the new crate owns the signed admission
types, `hft-live`, the alpha CLI, and the store consume it directly; targeted
governance, alpha, store, engine, and live checks pass. The next slice is
runtime-attribution ownership. Runtime and production readback remain unchanged.
