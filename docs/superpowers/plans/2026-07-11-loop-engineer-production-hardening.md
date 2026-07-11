# Loop Engineer Production Hardening Implementation Plan

**Spec:** `docs/superpowers/specs/2026-07-11-loop-engineer-production-hardening-design.md`

**Method:** Rust-first, focused package validation, one atomic commit per task. Do not run full workspace builds after every task. Live orders and provider-side secret rotation are outside local verification.

## Global Constraints

- No LLM, GP, MCTS, Bayesian, ML training, or RL call in the tick/order hot path.
- Research agents cannot place/cancel orders, load artifacts directly, resume trading, or weaken hard risk caps.
- Runtime hard limits always clamp proposed limits; live-small remains disabled.
- Reuse existing alpha-harness, SystemRuntime, OMS, Sentinel, execution worker, and strategy factory boundaries.
- Never print secret values in logs, tests, reports, or commits.
- Every money/security path leaves a focused runnable test.

### Task 1: Contain Tracked Secrets And Repair Supply-Chain Gates

**Files:** tracked credential/key files, root and Rust ignore files, `rust_hft/.cargo/audit.toml`, `.github/workflows/security-enabled.yml`, `rust_hft/Cargo.toml`, `rust_hft/Cargo.lock`.

- Remove private keys and populated credentials from the current tree; retain only explicit `.example` templates with inert placeholders.
- Exclude credentials and keys from Git and Docker contexts.
- Add a focused repository secret-presence check using existing shell/CI facilities.
- Fix cargo-audit policy syntax and update vulnerable direct/transitive dependencies where lockfile-compatible.
- Make security CI fail on audit/tool failure instead of parsing a missing or invalid report.
- Verify the secret check and `cargo audit`; commit.

### Task 2: Restore Feature-Matrix Builds And Canonical Deployment Artifacts

**Files:** Binance JSON converter/common adapter boundary, Dockerfiles, compose, Kubernetes trading deployment, root workflows.

- Fix the serde/simd JSON type split at the shared parser boundary.
- Build and run `hft-live`, not the collector, in the trading image.
- Correct entrypoint/command composition, health-check dependency, `/readiness`, config path, and durable deployment policy/key/nonce/audit/feedback mounts.
- Add focused default and relevant feature-matrix checks without reinstating per-change full-workspace compilation.
- Verify package checks and manifest/static container checks; commit.

### Task 3: Correct Portfolio And Risk Accounting

**Files:** `risk-control/portfolio-core`, account view contracts, default risk manager, focused tests.

- Implement exact signed-position accounting for open/increase/reduce/close/cross operations.
- Calculate equity and drawdown from cash plus marked positions with an initialized high-water mark.
- Stop treating fill notional as PnL; expose real metrics or explicit unavailable state.
- Add invariant tests for long, short, partial close, crossing, duplicate fills, marks, and losses before positive PnL.
- Verify only portfolio/risk packages; commit.

### Task 4: Make Emergency Cancellation And Reconciliation Fail Closed

**Files:** SystemRuntime control helpers, Sentinel worker, execution worker, execution client contract/adapters, focused tests.

- Move existing cancel-all logic into a reusable runtime control path and invoke it from Sentinel emergency handling.
- Prove emergency mode blocks new intents and cancellation commands reach execution clients.
- Detect local-only/exchange-only orders and client/list/parse failures; return unhealthy on incomplete reconciliation.
- Replace silent empty balance/open-order fallbacks with explicit unsupported/error results.
- Wire account reconciliation through the execution worker/runtime without sharing mutable clients across workers.
- Verify engine/runtime and affected adapters; commit.

### Task 5: Bind Promotion, Approval, Bundle, And Signature

**Files:** alpha-domain, alpha-store migrations/repositories, governance CLI, focused tests.

- Add typed Formula and ONNX `StrategyBundle` contracts with canonical hash validation.
- Mark opaque candidate variants research-only for promotion.
- Bind promotion to candidate hash, dataset manifest, evaluator version, sealed result, and bundle hash.
- Add approval validity/revocation/signer fields with backward-safe migration.
- Generate and sign envelopes only from persisted promotion/bundle records; reject arbitrary mismatches.
- Verify domain/store/harness governance tests; commit.

### Task 6: Load The Exact Strategy Bundle In Rust Runtime

**Files:** factor DSL/runtime strategy implementation, runtime strategy config/factory, deployment intake, `hft-live` tests.

- Add the minimum deterministic live Formula strategy over supported bar/snapshot fields and reuse the existing Factor AST.
- Map ONNX bundles to the existing DL strategy after size/hash/schema validation.
- Apply the bundle before `SystemBuilder::build`; bind envelope limits to strategy/runtime risk requests while retaining lower hard caps.
- Reject unsupported fields/operators/artifacts and duplicate strategy IDs.
- Gate direct resume, model replacement, strategy replacement, and risk increases behind the same deployment authority while preserving direct pause/degrade/emergency controls.
- Prove accepted paper/shadow handoffs instantiate the referenced strategy; commit.

### Task 7: Emit Real Attribution And Complete The Goal Loop

**Files:** alpha-domain/store/engine/app and `hft-live` feedback worker/tests.

- Add a typed `LoopRun` with target stage, stage records, completion policy, and terminal reason. Keep research-mission completion research-local.
- Add explicit mission completion criteria and persisted completion reasons.
- Persist versioned engine-specific checkpoint state and restore the MCTS/Bayesian frontier exactly.
- Stop the kernel on verified goal completion, pause, failure, or budget exhaustion; preserve checkpoint/resume.
- Add one `loop run` command that composes existing mission run and learning steps without execution authority.
- Subscribe to runtime execution events and portfolio snapshots; emit strategy-scoped fill/reject/cancel/PnL/drawdown attribution only after runtime start.
- Feed attributed failures into immutable learning directives and validator-gated policy revisions.
- Pin an adopted child search-policy revision into the next follow-up mission.
- Verify loop resume, completion, attribution, and no-risk-mutation tests; commit.

### Task 8: Harden Research Evaluation And Data Truth

**Files:** formula evaluator, evaluation contracts, data mission/source catalog, CLI, focused tests.

- Reject no-trade/zero-edge candidates and require minimum rows/trades, positive edge, drawdown bound, and adjusted score.
- Persist the additional metrics and evaluator configuration/version in promotion evidence.
- Distinguish catalog capability from governed acquisition support.
- Reject open/partial candles, duplicates, gaps, stale data, invalid OHLC, negative volume, and inconsistent manifests.
- Keep RL labeled as a lab search-policy engine until allocator trace criteria exist.
- Verify alpha engine/harness/collector packages; commit.

### Task 9: Final Production-Gate Verification And Truthful Documentation

**Files:** root README, Rust architecture/operator docs, CI/release documentation, progress ledger.

- Run the focused acceptance matrix once, then one release/feature graph validation.
- Build the production container and validate Kubernetes manifests without contacting a live venue.
- Re-run repository secret scan and cargo audit.
- Record exactly which external actions remain: credential rotation, public-history rewrite/cache purge, real venue testnet/shadow soak, and live-small approval.
- Correct capability labels and Loop Engineer definition; perform whole-branch review; commit.
