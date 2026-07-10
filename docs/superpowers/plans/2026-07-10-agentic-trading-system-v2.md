# Agentic Trading System v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the demo research/control plane with a persistent Rust-first AutoResearch harness while keeping all order execution inside the existing Rust live runtime.

**Architecture:** Add four focused crates under `rust_hft/alpha-harness/` and migrate useful DSL, evaluator, manifest, and risk contracts into them by reuse rather than reimplementation. DuckDB is the local control-plane source of truth; the research app emits only signed deployment envelopes, and `apps/live` verifies them before any runtime mutation. Old research crates and legacy paths are removed only after their replacement tests pass.

**Tech Stack:** Rust 2021, DuckDB 1.10504, serde/serde_json, chrono, SHA-256, Ed25519, existing market/data/risk/OMS/execution crates, focused Cargo package checks.

## Global Constraints

- No LLM, GP, MCTS, Bayesian, ML, or RL call in the per-tick or per-order hot path.
- Only `rust_hft/apps/live` and runtime-owned execution components may submit or cancel venue orders.
- Agents cannot weaken hard risk caps or authorize capital.
- Every quantitative result references immutable data, evaluator, prompt/model, code, and policy versions.
- DuckDB stores control-plane state; raw and large derived data remains in Parquet or trace artifacts.
- Sealed holdout data is inaccessible to active search engines.
- Python remains only behind `lab/python/` until a focused Rust parity test passes.
- Do not run full-workspace builds for ordinary changes; use the package commands named per task.
- Preserve unrelated user changes and make one atomic commit per task.

---

### Task 1: Finish and Correct the Existing DuckDB Replay Baseline

**Files:**
- Modify: `rust_hft/apps/agentic-alpha/src/main.rs`
- Modify: `docs/superpowers/specs/2026-07-10-agentic-trading-system-v2-design.md`

**Interfaces:**
- Consumes: existing `duckdb_agent_loop`, `walk_forward_split`, `FileFactorStore` code already present in the worktree.
- Produces: tested chronological holdout baseline, explicitly not mislabeled as walk-forward, that later tasks replace; approved spec status.

- [ ] **Step 1: Add focused baseline tests**

Rename `walk_forward_split`/`WalkForwardReport` to `chronological_holdout_split`/`ChronologicalHoldoutReport`. Add a `#[cfg(test)]` module that proves the slices are chronological and non-overlapping by position, rejects too-short replay data through `duckdb_agent_loop`, and round-trips a passed `FactorAsset` through the configured file store.

```rust
#[test]
fn chronological_holdout_split_is_non_overlapping() {
    let rows = replay_fixture_rows(10);
    let (train, validation, test) = chronological_holdout_split(&rows);
    assert_eq!((train.len(), validation.len(), test.len()), (6, 2, 2));
    assert!(std::ptr::eq(train.last().unwrap(), &rows[5]));
    assert!(std::ptr::eq(validation.first().unwrap(), &rows[6]));
    assert!(std::ptr::eq(test.first().unwrap(), &rows[8]));
}
```

- [ ] **Step 2: Run the focused test**

Run: `cargo test -p hft-agentic-alpha chronological_holdout --locked`

Expected: the new test passes without compiling unrelated workspace packages.

- [ ] **Step 3: Mark the design approved**

Change the design status from `Draft for written-spec review` to `Approved for implementation` and add this plan path under the implementation gate.

- [ ] **Step 4: Commit the baseline**

```bash
git add rust_hft/apps/agentic-alpha/src/main.rs docs/superpowers/specs/2026-07-10-agentic-trading-system-v2-design.md
git commit -m "test: freeze DuckDB agent loop baseline"
```

### Task 2: Seal the Research/Execution Boundary and Correct Repository Truth

**Files:**
- Modify: `rust_hft/apps/agentic-alpha/src/main.rs`
- Modify: `rust_hft/apps/agentic-alpha/Cargo.toml`
- Modify: `README.md`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: existing research demo commands.
- Produces: a research binary with no execution adapter, `OrderIntent`, raw transaction broadcast, or live confirmation flag; accurate top-level status; focused root CI for ordinary changes.

- [ ] **Step 1: Add a source-boundary check**

Create one repository test script assertion in the app test module that reads its own source and rejects the forbidden command names and imports after removal:

```rust
#[test]
fn research_binary_has_no_live_actuation_surface() {
    let source = include_str!("main.rs");
    for forbidden in ["BinanceOrder", "EvmRawTx", "ExecutionClient", "OrderIntent"] {
        assert!(!source.contains(forbidden), "forbidden research authority: {forbidden}");
    }
}
```

- [ ] **Step 2: Remove direct actuation code**

Delete `BinanceOrder`, `EvmRawTx`, their reports/functions/CLI arms, and imports of execution adapters, order types, signing credentials, and execution ports. Retain read-only connectivity diagnostics only if they do not require credentials or broadcast payloads.

- [ ] **Step 3: Remove now-unused dependencies**

Remove `execution_adapter_binance`, `hft-core`, `integration`, `ports`, and `tokio` from `apps/agentic-alpha/Cargo.toml` when `cargo machete`-style source search confirms no remaining use. Keep `reqwest` only for read-only connectivity.

- [ ] **Step 4: Verify and commit**

Replace top-level production/completion claims with `implemented`, `simulated`, `deferred`, and `live-capable` labels. Change the ordinary root Rust CI job from `cargo build --workspace --locked` to package-scoped checks for the execution and alpha lanes; keep full graph validation only in release jobs.

Run: `cargo test -p hft-agentic-alpha research_binary_has_no_live_actuation_surface --locked`

Run: `cargo check -p hft-agentic-alpha --locked`

Run: `rg -n 'cargo (build|check|test) --workspace' .github/workflows/ci.yml` (expected: no matches)

```bash
git add rust_hft/apps/agentic-alpha README.md .github/workflows/ci.yml
git commit -m "refactor: seal agent research execution boundary"
```

### Task 3: Add Alpha Domain Contracts and Signed Deployment Envelope

**Files:**
- Create: `rust_hft/alpha-harness/domain/Cargo.toml`
- Create: `rust_hft/alpha-harness/domain/src/lib.rs`
- Modify: `rust_hft/Cargo.toml`

**Interfaces:**
- Consumes: `hft-factor-dsl::FactorAst`, existing manifest identifiers, serde-compatible artifact references.
- Produces: `ResearchMission`, `ResearchIteration`, `CandidateArtifact`, `DeploymentEnvelope`, `SignedDeploymentEnvelope`, and validation APIs.

- [ ] **Step 1: Add failing domain validation tests**

Cover empty IDs, zero budgets, invalid lifecycle transitions, expired envelopes, payload hash mismatch, unknown key, invalid signature, and duplicate nonce.

```rust
#[test]
fn forged_envelope_is_rejected() {
    let (signed, trusted) = signed_fixture();
    let mut forged = signed.clone();
    forged.envelope.max_notional += 1;
    assert_eq!(verify_envelope(&forged, &trusted, now()).unwrap_err(),
               DomainError::PayloadHashMismatch);
}
```

- [ ] **Step 2: Implement minimal typed contracts**

Use enums for validator mode, mission status, iteration verdict, candidate payload, approval class, and allowed intent type. Validate all untrusted strings, finite/non-negative monetary limits, `valid_from < expires_at`, non-empty instruments, and exact account/venue binding.

```rust
pub enum CandidateArtifact {
    Formula(FactorAst),
    Program(serde_json::Value),
    ModelConfig(serde_json::Value),
    ModelArtifact(ArtifactRef),
    Ensemble(serde_json::Value),
    AllocatorPolicy(serde_json::Value),
}
```

- [ ] **Step 3: Implement canonical signing and verification**

Serialize the unsigned envelope to canonical JSON with deterministic map ordering, hash with SHA-256, and sign the hash with Ed25519. Verification checks hash, signature, trusted key ID, time window, runtime/account/venue binding, hard limits, and an injected nonce lookup before returning `VerifiedDeploymentEnvelope`.

- [ ] **Step 4: Add the crate without changing default members**

Add `alpha-harness/domain` to workspace members and workspace dependencies for already-locked `sha2`, `ed25519-dalek`, `hex`, and `base64`. Do not add it to `default-members`.

- [ ] **Step 5: Verify and commit**

Run: `cargo test -p alpha-domain --locked`

```bash
git add rust_hft/Cargo.toml rust_hft/Cargo.lock rust_hft/alpha-harness/domain
git commit -m "feat: add alpha mission and deployment contracts"
```

### Task 4: Add Transactional DuckDB Control-Plane Store

**Files:**
- Create: `rust_hft/alpha-harness/store/Cargo.toml`
- Create: `rust_hft/alpha-harness/store/src/lib.rs`
- Create: `rust_hft/alpha-harness/store/migrations/001_control_plane.sql`
- Modify: `rust_hft/Cargo.toml`

**Interfaces:**
- Consumes: `alpha_domain::{ResearchMission, ResearchIteration, CandidateArtifact, SignedDeploymentEnvelope}`.
- Produces: `AlphaStore::open`, mission/iteration/checkpoint repositories, append-only journals, factor/model/policy records, memory events, approval records, and durable nonce consumption.

- [ ] **Step 1: Write repository tests against temporary DuckDB files**

Prove: migration idempotency, mission round-trip, append-only iteration rejection on duplicate ID, atomic checkpoint with budget update, resume from last completed iteration, complete lineage query, and nonce consume-once semantics across reopen.

```rust
#[test]
fn nonce_is_durable_and_single_use() {
    let path = temp_db();
    AlphaStore::open(&path).unwrap().consume_nonce("n-1", now()).unwrap();
    let reopened = AlphaStore::open(&path).unwrap();
    assert_eq!(reopened.consume_nonce("n-1", now()).unwrap_err(), StoreError::NonceReplay);
}
```

- [ ] **Step 2: Create the minimal normalized schema**

Create tables for schema migrations, missions, iterations, candidate artifacts, evaluation artifacts, registry revisions, research memory, approvals, checkpoints, budget usage, deployment envelopes, and consumed nonces. Store typed payloads as validated JSON plus content hash; use foreign keys and unique IDs where DuckDB supports them.

- [ ] **Step 3: Implement transactional repositories**

Every public write starts a DuckDB transaction, validates domain input, writes the record and journal event together, and commits only after row-count checks. No file-backed JSON fallback is allowed.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p alpha-store --locked`

```bash
git add rust_hft/Cargo.toml rust_hft/Cargo.lock rust_hft/alpha-harness/store
git commit -m "feat: add DuckDB alpha control-plane store"
```

### Task 5: Add the Resumable AutoResearch Kernel

**Files:**
- Create: `rust_hft/alpha-harness/engine/Cargo.toml`
- Create: `rust_hft/alpha-harness/engine/src/lib.rs`
- Create: `rust_hft/alpha-harness/engine/src/evaluation.rs`
- Modify: `rust_hft/Cargo.toml`

**Interfaces:**
- Consumes: `AlphaStore`, domain mission/iteration contracts, existing `FactorAst`, evaluator math, and manifest types.
- Produces: `AutoResearchKernel::run`, `EngineContext`, `ProposalEngine`, `CandidateEvaluator`, deterministic checkpoints, and persisted keep/discard/crash decisions.

- [ ] **Step 1: Write resume and budget tests**

Use deterministic in-process test engines. Prove that a restart does not rerun completed iterations, every attempted iteration is persisted before the next starts, crashes remain queryable, and candidate/expansion/token/time budgets stop the mission.

```rust
#[test]
fn resume_skips_completed_iterations() {
    let calls = AtomicUsize::new(0);
    run_fixture_mission(&calls, StopAfter::Iteration(2));
    resume_fixture_mission(&calls);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}
```

- [ ] **Step 2: Implement the mission state machine**

Use one explicit loop with persisted state transitions: `Pending -> Running -> Paused|Completed|BudgetExhausted|Failed`. Each iteration performs proposal, validation, evaluation, verdict persistence, checkpoint, then budget accounting. The kernel has no execution imports.

- [ ] **Step 3: Implement purged walk-forward evaluation**

Replace the single 60/20/20 baseline with expanding or rolling folds configured by train/validation/test lengths, purge rows, embargo rows, and sealed holdout ID. Reject non-monotonic `available_time`, overlap, insufficient coverage, and attempts to expose holdout rows through `EngineContext`.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p alpha-engine resume --locked`

Run: `cargo test -p alpha-engine walk_forward --locked`

```bash
git add rust_hft/Cargo.toml rust_hft/Cargo.lock rust_hft/alpha-harness/engine
git commit -m "feat: add resumable autoresearch kernel"
```

### Task 6: Replace Simulated Search Labels with Real Deterministic Engines

**Files:**
- Create: `rust_hft/alpha-harness/engine/src/engines/mod.rs`
- Create: `rust_hft/alpha-harness/engine/src/engines/gp.rs`
- Create: `rust_hft/alpha-harness/engine/src/engines/mcts.rs`
- Create: `rust_hft/alpha-harness/engine/src/engines/bayesian.rs`
- Create: `rust_hft/alpha-harness/engine/src/engines/offline_rl.rs`
- Create: `rust_hft/alpha-harness/engine/src/llm.rs`
- Create: `rust_hft/alpha-harness/engine/tests/llm_live.rs`

**Interfaces:**
- Consumes: `ProposalEngine`, `EngineContext`, Factor AST grammar, persisted evaluation rewards and trace history.
- Produces: reproducible GP/MCTS/Bayesian candidates, bounded LLM hypotheses/failure explanations, and lab-only offline RL proposals.

- [ ] **Step 1: Add deterministic engine contract tests**

For a fixed seed and fixture, assert identical candidate IDs, lineage, budgets, and traces. Assert each engine rejects missing required data and never labels synthetic fixture results as live evidence.

- [ ] **Step 2: Implement GP**

Port the useful existing GP mutation/crossover behavior behind `ProposalEngine`; enforce AST validity, maximum depth, novelty deduplication, seeded selection, and evaluation-budget accounting.

- [ ] **Step 3: Implement MCTS**

Implement UCT selection, grammar-bounded expansion, evaluator reward, and backpropagation. Persist every node's parent, visits, total reward, best reward, action, and candidate ID.

- [ ] **Step 4: Implement Bayesian optimization**

For bounded numeric factor parameters, use a deterministic Gaussian-process surrogate and expected-improvement acquisition over a finite candidate grid. Start with Latin-hypercube seeded points and persist observations/acquisition scores.

- [ ] **Step 5: Implement bounded LLM calls**

Use an OpenAI-compatible HTTP endpoint configured by environment variables. Require JSON-schema output for `HypothesisArtifact` and `FailureExplanation`, record provider/model/prompt hashes/token usage, redact credentials, and return an explicit unavailable error instead of a synthetic response.

Add an ignored, opt-in integration test that makes one real bounded call and writes the returned hypothesis artifact to a temporary file. It fails, rather than skips or fabricates output, when explicitly run without endpoint/model/key configuration.

Run when credentials are available: `cargo test -p alpha-engine --test llm_live --locked -- --ignored`

Expected: one valid `HypothesisArtifact` with non-empty provider/model/prompt hash and real token-usage metadata.

- [ ] **Step 6: Implement offline RL gating**

Implement a discrete offline Q-learning policy over persisted search actions and rewards. Refuse training below configurable minimum trace count, emit only lab proposals, and persist dataset/policy versions. Do not add online updates.

- [ ] **Step 7: Verify and commit**

Run: `cargo test -p alpha-engine engines --locked`

Run: `cargo test -p alpha-engine llm_schema --locked`

```bash
git add rust_hft/alpha-harness/engine
git commit -m "feat: add real alpha search engines"
```

### Task 7: Add Data Missions and the Replacement Harness App

**Files:**
- Create: `rust_hft/alpha-harness/app/Cargo.toml`
- Create: `rust_hft/alpha-harness/app/src/main.rs`
- Create: `rust_hft/alpha-harness/app/src/data_mission.rs`
- Create: `rust_hft/tools/collector/src/lib.rs`
- Create: `rust_hft/tools/collector/src/source_catalog.rs`
- Create: `rust_hft/tools/collector/tests/data_mission_smoke.rs`
- Modify: `rust_hft/Cargo.toml`
- Modify: `rust_hft/tools/collector/src/main.rs`
- Modify: `rust_hft/tools/collector/Cargo.toml`

**Interfaces:**
- Consumes: existing collector/data-pipeline connectors through a reusable `hft_collector` library target, `AlphaStore`, `AutoResearchKernel`, engine registry.
- Produces: `alpha-harness mission create|run|resume|status`, `data acquire`, `candidate list`, `evaluate`, `promote`, and `deployment sign` commands with no order command.

- [ ] **Step 1: Add CLI and data-catalog tests**

Assert command parsing, no execution verbs, source capability discovery, explicit fixture mode, and rejection of silent real-to-synthetic fallback.

- [ ] **Step 2: Register existing connector capabilities**

Extract only source discovery and one-shot acquisition orchestration from the bin-only collector into `src/lib.rs`; keep CLI/bootstrap in `main.rs`. Expose LOB, trade, BBO, OHLCV, funding, open-interest, and listing capabilities only when an existing connector actually implements them. A `DataAcquisitionMission` records source, symbols, requested interval, destination artifact, and quality requirements.

- [ ] **Step 3: Produce content-addressed dataset manifests**

On successful acquisition, hash the artifact, record schema and point-in-time fields (`event_time`, `exchange_time`, `receive_time`, `available_time`, `ingestion_time`), and persist quality counts. Failed acquisition creates a failure artifact and never substitutes fixtures.

Add an ignored opt-in public-connector smoke that acquires a bounded real sample through the extracted library and asserts a non-empty quality report plus a matching content-addressed dataset manifest.

- [ ] **Step 4: Wire the app to mission execution**

The app opens one DuckDB database, resumes existing state, selects only explicitly enabled engines, and emits structured JSON status suitable for an Agent tool. LLM credentials remain process environment inputs and are never persisted.

- [ ] **Step 5: Verify and commit**

Run: `cargo test -p alpha-harness --locked`

Run: `cargo check -p hft-collector --locked`

Run when network access is enabled: `cargo test -p hft-collector --test data_mission_smoke --features collector-binance --locked -- --ignored`

```bash
git add rust_hft/Cargo.toml rust_hft/Cargo.lock rust_hft/alpha-harness/app rust_hft/tools/collector
git commit -m "feat: add agent-driven alpha harness app"
```

### Task 8: Route Signed Deployments Through the Rust Live Runtime

**Files:**
- Create: `rust_hft/apps/live/src/deployment_envelope.rs`
- Modify: `rust_hft/apps/live/src/main.rs`
- Modify: `rust_hft/apps/live/Cargo.toml`
- Modify: `rust_hft/alpha-harness/app/src/main.rs`

**Interfaces:**
- Consumes: `SignedDeploymentEnvelope`, runtime trusted keys, current account/venue/instrument state, and a runtime-owned durable nonce ledger.
- Produces: verified paper/shadow/live-small deployment requests accepted only by `apps/live` before existing risk/OMS/execution paths.

- [ ] **Step 1: Add runtime rejection tests**

Test forged payload, invalid signature, unknown key, expiry, early validity, wrong account, wrong venue, wrong runtime config/risk hash, nonce replay after restart, and requested limits above current runtime caps.

- [ ] **Step 2: Add envelope intake**

Add a cold-path CLI/file or control-service intake that verifies the envelope before translating it to existing runtime configuration. Implement `RuntimeNonceLedger` inside `apps/live`: append and `sync_data` each accepted nonce to a runtime-owned file before actuation, load prior nonces on startup, and reject duplicates across restart. Do not use or import the control-plane `AlphaStore`. Verification cannot resume a paused runtime or increase caps without the required approval class.

- [ ] **Step 3: Recheck current state**

After cryptographic verification, compare account, venue, instrument allowlist, notional, symbol exposure, order size, slippage, and runtime/risk hashes against current Rust state. Record accept/reject attribution before any model/policy activation.

Add a positive integration test for a valid envelope that reaches existing `apps/live` paper and shadow activation adapters, records both accepted transitions, and never imports an execution adapter into `alpha-harness`.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p alpha-domain deployment_envelope --locked`

Run: `cargo test -p hft-live deployment_envelope --locked`

Run: `cargo test -p hft-live accepted_paper_shadow_handoff --locked`

Run: `cargo check -p hft-live --locked`

```bash
git add rust_hft/apps/live rust_hft/alpha-harness/app
git commit -m "feat: verify signed alpha deployments in live runtime"
```

### Task 9: Close the Feedback and Learning Loop

**Files:**
- Modify: `rust_hft/alpha-harness/domain/src/lib.rs`
- Modify: `rust_hft/alpha-harness/store/src/lib.rs`
- Modify: `rust_hft/alpha-harness/engine/src/lib.rs`
- Modify: `rust_hft/apps/live/src/deployment_envelope.rs`

**Interfaces:**
- Consumes: paper/shadow/live-small attribution, rollback/decay events, failed iterations, versioned search policy.
- Produces: immutable research-memory events, repeated-failure missions, policy comparison evidence, and gated offline RL updates.

- [ ] **Step 1: Add attribution-to-mission tests**

Prove runtime events append without mutating old records, repeated classified failures create one idempotent follow-up mission, and a search-policy revision is adopted only when its validator result beats the prior revision.

- [ ] **Step 2: Persist feedback events and policy revisions**

Use append-only events keyed by deployment/asset revision and content hash. Store policy parent revision, learning evidence, validator result, adoption status, and rollback reason.

- [ ] **Step 3: Enforce bounded autonomy**

Require an existing same-class human approval record before auto-promotion can request live-small. RL and LLM outputs may alter lab search priorities or propose bounded allocator weights but cannot change runtime caps.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p alpha-engine learning --locked`

Run: `cargo test -p alpha-store research_memory --locked`

```bash
git add rust_hft/alpha-harness rust_hft/apps/live/src/deployment_envelope.rs
git commit -m "feat: close bounded alpha learning loop"
```

### Task 10: Remove Replaced Legacy Paths and Correct Documentation

**Files:**
- Remove only after replacement checks pass: paths listed in design section 15.3.
- Modify: `rust_hft/Cargo.toml`
- Modify: root `README.md` and active architecture/status documents discovered by `rg`.
- Modify: `.github/workflows/*.yml`
- Create: `rust_hft/alpha-harness/README.md`

**Interfaces:**
- Consumes: all passing replacement crates and runtime integration.
- Produces: one active research/control plane, one deployment path, accurate capability labels, and focused CI/package commands.

- [ ] **Step 1: Verify every deletion prerequisite**

For each removal candidate, use `rg` and `cargo metadata --no-deps` to prove no active import, workspace member, deployment reference, CI command, or documented operator path remains. Do not delete `ml_trainer/`, Python prototypes, or `apps/binance-md/` without their explicit parity evidence.

- [ ] **Step 2: Remove unsafe and dead paths in bounded commits**

First remove `control_ws/`, legacy `deployment/`, `rust_hft/tools/hft-agent/`, direct research actuation remnants, dead collector backup files, and inactive nested workflows. Then remove static prototype wrappers only after real engine parity. Keep each logically independent deletion in its own commit.

- [ ] **Step 3: Replace misleading documentation**

Finish the truth correction started in Task 2 across active architecture/status documents. Document only four capability states: `implemented`, `simulated`, `deferred`, `live-capable`. Describe DuckDB as control-plane truth, ClickHouse as analytics, Parquet/traces as data artifacts, and `apps/live` as execution owner. Consolidate active Rust CI at the repository root; remove nested workflow copies only after every retained job has a root equivalent. Ordinary CI uses package-scoped checks, while release workflows may validate the full workspace graph.

- [ ] **Step 4: Run final focused and workspace-graph validation**

Run: `cargo test -p alpha-domain --locked`

Run: `cargo test -p alpha-store --locked`

Run: `cargo test -p alpha-engine --locked`

Run: `cargo test -p alpha-harness --locked`

Run: `cargo check -p hft-live -p hft-collector --locked`

Run once after workspace-member removals: `cargo metadata --locked --no-deps`

- [ ] **Step 5: Commit documentation and final graph cleanup**

```bash
git add -A
git commit -m "refactor: retire replaced agent control planes"
```

## Completion Evidence

The branch is complete only when all 14 acceptance criteria in the approved design have a command, persisted artifact, or focused test proving them. Final review must inspect the full branch diff from `15d21d42`, verify the research binary has no execution dependency, and confirm that no synthetic fallback is presented as real research evidence.
