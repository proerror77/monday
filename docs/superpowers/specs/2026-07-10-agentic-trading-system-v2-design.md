# Agentic Trading System v2 Design

Date: 2026-07-10
Status: Draft for written-spec review

## 1. Decision

Rewrite the Agentic Alpha research and control plane while preserving the existing Rust execution plane.

The system will autonomously acquire research data, formulate hypotheses, generate and evaluate strategy candidates, retain research memory, and propose bounded deployments. LLMs, MCTS, GP, Bayesian optimization, ML, and RL remain outside the trading hot path. The existing Rust runtime remains the only component allowed to perform low-latency inference, risk checks, order lifecycle management, and venue execution.

This is a controlled replacement, not a whole-repository rewrite.

## 2. Required Outcomes

The completed system must:

1. Pull registered market and external data through Rust-owned connectors.
2. Record point-in-time data availability and immutable dataset manifests.
3. Run persistent, budgeted, validator-gated research missions.
4. Use LLM agents for hypothesis generation, candidate authoring, failure explanation, and research critique.
5. Use real GP, MCTS, Bayesian, ML, and RL implementations through one proposal contract.
6. Evaluate candidates with purged walk-forward validation, realistic costs, and sealed holdouts.
7. Persist candidates, experiments, factors, models, policies, failures, and lineage in DuckDB for local operation.
8. Promote only immutable, manifest-backed assets through deterministic gates.
9. Hand approved deployments to the Rust runtime through a signed, replay-protected envelope.
10. Feed paper, shadow, and live-small attribution back into future research missions.

## 3. Non-Goals

- No LLM call in the per-tick or per-order hot path.
- No online code modification inside the live runtime.
- No autonomous weakening of hard risk caps.
- No direct Agent call to exchange execution adapters.
- No immediate support for every data source and every search engine in the first vertical slice.
- No forced removal of useful Python model-training code before contract parity exists.
- No new microservice mesh or event platform until a single-process harness reaches its limits.

## 4. Existing Repository Assessment

The repository contains four overlapping generations:

1. Legacy Python/Agno control-plane code.
2. Rust-centric HFT execution, risk, and market-data infrastructure.
3. Python quantitative and model-training prototypes.
4. A Rust Agentic Alpha skeleton whose contracts are useful but whose engines and learning loop are mostly demonstrations.

The current overlap creates contradictory documentation, duplicate deployment paths, fake engine labels, multiple storage truths, and unsafe execution ownership.

## 5. Invariants

### 5.1 Execution ownership

Only `apps/live` and runtime-owned execution components may submit or cancel venue orders.

The research harness may produce factor assets, model artifacts, allocator policies, bounded risk-policy proposals, and signed deployment requests. It may not instantiate venue execution clients or broadcast raw transactions.

### 5.2 Fail-safe ownership

Rust Sentinel and hard risk checks may always pause, degrade, cancel, or stop trading without LLM approval.

Resume trading, load a new model, change risk configuration, increase exposure, or create a new live permission class requires deterministic validation and an approval artifact.

### 5.3 Research integrity

- Every result references exact data, feature, label, evaluator, prompt, model, code, and policy versions.
- Search and tuning may use training and validation folds.
- A sealed holdout may be evaluated only by the promotion workflow.
- Holdout results do not feed the active search mission.
- Paper, shadow, and live results may create later missions, but never rewrite historical artifacts.

## 6. Target Architecture

```text
Rust data connectors
  -> raw Parquet artifacts / ClickHouse realtime analytics
  -> point-in-time Data Catalog
  -> content-addressed Dataset Manifest
  -> Research Mission
  -> AutoResearch Kernel
       LLM Hypothesis Agent
       GP / MCTS / Bayesian / ML / RL engines
       deterministic evaluator
       keep / discard / crash decision
  -> Candidate Pool
  -> sealed promotion evaluation
  -> Factor Bank / Model Registry / Policy Registry
  -> deterministic promotion gate
  -> signed DeploymentEnvelope
  -> existing apps/live Rust runtime
  -> risk / Sentinel / OMS / execution gateway
  -> attribution and decay events
  -> Research Memory
  -> next Research Mission
```

## 7. Minimal Rust Structure

Do not add one crate per noun. Use four durable packages and the existing runtime:

```text
rust_hft/alpha-harness/
  domain/    # immutable contracts and validation
  store/     # DuckDB repositories and migrations
  engine/    # mission loop, evaluators, search-engine adapters
  app/       # CLI/service entry point

rust_hft/apps/live/               # retained execution owner
rust_hft/tools/collector/         # retained data acquisition owner
rust_hft/market-core/             # retained hot runtime
rust_hft/risk-control/            # retained risk and Sentinel
rust_hft/execution-gateway/       # retained venue execution
```

Heavy ML dependencies remain outside `default-members`. Ordinary changes validate only directly affected packages.

## 8. Domain Contracts

### 8.1 ResearchMission

```text
mission_id
objective
hypothesis_scope
mutable_scope
dataset_manifest_id
baseline_artifact_id
validation_mode
validator_spec
search_budget
prompt_snapshot_id
search_policy_snapshot_id
status
created_at
updated_at
```

`validation_mode` is exactly one of:

- `mission_validator`: deterministic metric and invariant checks;
- `architect_artifact`: qualitative review with a required structured approval artifact.

Quantitative factor, model, and policy promotion missions use `mission_validator`. `architect_artifact` is limited to hypothesis quality, taxonomy, prompt, and harness-design research; it cannot authorize capital.

### 8.2 ResearchIteration

```text
iteration_id
mission_id
parent_candidate_ids
engine
hypothesis
candidate_artifact_id
evaluation_artifact_id
budget_usage
verdict: keep | discard | crash
failure_class
failure_explanation
created_at
```

Every iteration is append-only. A failed or discarded iteration remains queryable.

### 8.3 CandidateArtifact

Candidate payloads are typed:

```text
Formula(FactorAst)
Program(ProgramAst)
ModelConfig(ModelSpec)
ModelArtifact(ModelArtifactRef)
Ensemble(EnsembleSpec)
AllocatorPolicy(AllocatorPolicySpec)
```

The Factor Bank stores signal-producing factor revisions. Models and allocator policies have separate registries because their lifecycle and validation differ.

### 8.4 DeploymentEnvelope

```text
deployment_id
asset_revision_id
promotion_manifest_hash
runtime_config_hash
risk_policy_hash
account_id
venue
instruments
allowed_intent_types
max_notional
max_symbol_exposure
max_order_size
max_slippage_bps
valid_from
expires_at
nonce
approval_class
approval_signatures
payload_hash
```

The envelope payload is canonical JSON hashed with SHA-256 and signed with Ed25519. The runtime owns the trusted public-key set and a durable nonce ledger. It rejects expired, replayed, unsigned, mismatched, or over-limit envelopes, then runs risk checks again against current account and market state.

## 9. Data Acquisition

Agents do not implement exchange WebSocket clients. They create `DataAcquisitionMission` requests against a discoverable source catalog. Rust connectors execute those requests.

The first supported domains are:

1. LOB snapshots and deltas.
2. Tick trades and BBO.
3. OHLCV.
4. Funding and open interest where current connectors provide them.
5. Listing events from the existing listing monitor.

On-chain, liquidation, news, sentiment, and other external signals enter later through the same event envelope.

Every event records:

```text
event_time
exchange_time
receive_time
available_time
ingestion_time
source
schema_version
quality_flags
```

Low-frequency external events are not duplicated onto every tick. Feature queries use point-in-time as-of joins based on `available_time`.

## 10. AutoResearch Kernel

The kernel adopts validator-gated automated research:

1. One mission has one objective and one validator mode.
2. Mutable scope is explicit and narrow.
3. Evaluator code and sealed data are immutable during the mission.
4. Each iteration makes one attributable candidate change.
5. Each engine receives fixed candidate, expansion, token, time, and compute budgets.
6. Every result is persisted before the next iteration.
7. Improvement means validator evidence, not an Agent statement.
8. Crashes and negative results remain research memory.
9. Checkpoints allow pause and resume without regenerating completed work.
10. Completion requires a passing artifact or explicit budget exhaustion.

The program database stores kept and discarded candidates, novelty descriptors, lineage, and scores. Parent selection may use quality-diversity or a bandit scheduler, but selection logic is versioned and reproducible.

## 11. Agent Roles and Tools

Use one orchestrator with bounded roles, not an unrestricted multi-agent mesh:

- `HypothesisAgent`: formulates testable hypotheses from data catalog and memory.
- `CandidateAgent`: translates a hypothesis into DSL, program, model, or policy candidates.
- `FailureCritic`: explains evaluator failures and proposes a bounded next experiment.
- `ResearchArchitect`: validates qualitative harness changes only.

Agents receive available data, active missions, candidate lineage, evaluation history, budgets, tool permissions, and hard constraints.

Agents use primitive research tools:

```text
list_data_sources
create_data_mission
query_manifest
list_candidates
submit_candidate
run_evaluation
read_evaluation
record_hypothesis
record_failure_explanation
complete_iteration
```

Live execution tools are not available to research agents.

## 12. Search Engines

### 12.1 First vertical slice

Implement in this order:

1. Existing Python GP behavior ported or wrapped behind the real proposal protocol.
2. LLM hypothesis and failure-critic calls.
3. Real MCTS over Factor AST with selection, expansion, evaluation, and backpropagation.
4. Bayesian parameter optimization.

### 12.2 RL timing

RL generator and allocator are not trained on synthetic success labels. They start after enough persisted experiment and paper/shadow traces exist to define a measurable environment.

RL may generate lab proposals, select search directions, or propose a bounded allocator policy after offline validation. No online weight update occurs in the live execution process.

## 13. Evaluation

The evaluator must support:

- temporal ordering and point-in-time availability checks;
- purged and embargoed walk-forward folds;
- fees, slippage, funding, latency, and turnover;
- IC, RankIC, ICIR, Sharpe, drawdown, hit rate, and capacity estimates;
- regime and symbol stability;
- correlation and novelty against existing factors;
- minimum sample and event coverage;
- deterministic seeds and evaluator versions.

Evaluation stages are:

```text
syntax/type validation
  -> quick train-fold screening
  -> multi-fold walk-forward validation
  -> sealed holdout promotion test
  -> paper
  -> shadow
  -> bounded live-small
```

A single 60/20/20 split is not called walk-forward validation.

## 14. Storage

### 14.1 Local and single-process

DuckDB is the authoritative control-plane store for missions, iterations, candidate lineage, evaluation summaries, Factor Bank revisions, model and policy records, research memory, approval state, run checkpoints, and budget usage.

Raw and large derived datasets remain Parquet or trace artifacts. DuckDB stores paths, hashes, schemas, and indexes rather than duplicating every raw event.

### 14.2 Shared analytics

ClickHouse remains the realtime and shared analytics backend for market events, aligned features, metrics, and operational queries. It is not the mutable source of truth for approvals or lifecycle transitions.

### 14.3 Integrity

- Critical records are append-only or versioned.
- Artifact checksums are mandatory.
- Writes are transactional.
- Status transitions are idempotent.
- IDs are not accepted as proof without matching content hashes.

## 15. Preserve, Replace, and Remove

### 15.1 Preserve

- `rust_hft/market-core/`
- `rust_hft/data-pipelines/`
- `rust_hft/risk-control/`
- `rust_hft/execution-gateway/`
- `rust_hft/apps/live/`
- `rust_hft/apps/paper/`
- `rust_hft/apps/replay/`
- `rust_hft/apps/backtest/`
- `rust_hft/tools/collector/`
- `rust_hft/tools/listing-monitor/`
- `rust_hft/strategy-framework/infer-onnx/`
- `rust_hft/infra-services/core/model-manager/`
- canonical deployment assets under `deploy/`

`rust_hft/apps/binance-md/` remains until a focused behavior and latency comparison proves that the canonical collector replaces it.

### 15.2 Replace

- `rust_hft/apps/agentic-alpha/`
- current `rust_hft/research-core/` orchestration and storage-facing contracts
- file-backed artifact, experiment, and factor stores
- free-form approval references
- static prototype wrappers
- root architecture documentation and status claims
- root CI jobs that compile the entire workspace for narrow changes

Useful DSL, evaluator math, validation tests, and status contracts may be migrated rather than reimplemented.

### 15.3 Remove after replacement prerequisites pass

- `control_ws/`
- legacy Agno deployment under `deployment/`
- `rust_hft/tools/hft-agent/`
- `rust_hft/config/agent.toml`
- `rust_hft/research-core/prototype-adapter/`
- direct Binance order and EVM broadcast commands from the research app
- `rust_hft/tools/collector/src/main_backup.rs`
- `rust_hft/tools/collector/src/main_bitget_legacy.rs`
- `rust_hft/tools/collector/src/multi_collector.rs`
- `rust_hft/tools/collector/src/marketstream_runner.rs`
- `_archive/`
- `hft_ui_nextjs/`
- `test_bitget_standalone/`
- malformed and backup specs such as `specs/001-/` and `spec.md.backup`
- inactive nested `.github/workflows` directories
- obsolete Agno, OMX, production-ready, and completed-architecture reports

### 15.4 Python migration

Keep selected real GP, factor generation, Bayesian, RL, deep-model training, and feature parity implementations under an explicit `lab/python/` compatibility boundary until parity exists.

`ml_trainer/` is removed only after its useful training path is consolidated or replaced. Silent fallback from real data to synthetic data is prohibited; synthetic mode must be explicit.

## 16. Migration Sequence

### Phase 0: Safety and truth

- Disable and remove legacy LLM mutation authority.
- Remove direct live actuation from the research app.
- Mark current engines and prototype wrappers as simulated until replaced.
- Replace misleading top-level documentation.

### Phase 1: Repository cleanup

- Remove dead and superseded paths in separate atomic commits.
- Consolidate deployment under `deploy/`.
- Consolidate active CI at repository root.
- Preserve historical recovery through Git, not archive directories.

### Phase 2: New domain and store

- Add the four-package `alpha-harness` structure.
- Add DuckDB migrations and repositories.
- Implement content-addressed manifests and append-only run journal.
- Add focused repository tests.

### Phase 3: Data mission vertical slice

- Register existing collector capabilities.
- Persist dataset manifests and quality reports.
- Produce one reproducible LOB/tick/OHLCV/funding research snapshot.

### Phase 4: AutoResearch vertical slice

- Add mission creation, checkpoint/resume, budget accounting, validator artifacts, and keep/discard/crash history.
- Run real GP and LLM hypothesis/critic candidates against deterministic fixtures.

### Phase 5: Real search and evaluation

- Add real MCTS and Bayesian engines.
- Add purged walk-forward and sealed holdout evaluation.
- Persist accepted factors and rejected research evidence.

### Phase 6: Runtime handoff

- Add signed `DeploymentEnvelope` production and verification.
- Route deployment through `apps/live` and existing risk/OMS/execution components.
- Validate idempotency, expiry, nonce replay protection, and current-state risk checks.

### Phase 7: Feedback and bounded autonomy

- Export paper/shadow/live attribution and decay events.
- Create new missions from repeated failures.
- Require first same-class human approval before bounded live-small auto-promotion.
- Add RL search policy and allocator only after sufficient trace history exists.

## 17. Validation Lanes

Do not validate ordinary work with full-workspace builds.

```text
domain change:
  cargo test -p alpha-domain --locked

store change:
  cargo test -p alpha-store --locked

engine change:
  cargo test -p alpha-engine <focused-test> --locked

app wiring:
  cargo check -p alpha-harness --locked

runtime envelope change:
  cargo test -p alpha-domain deployment_envelope --locked
  cargo check -p hft-live --locked

collector integration:
  cargo check -p hft-collector --locked
  focused connector fixture or smoke
```

Full workspace validation is reserved for dependency-graph changes, release candidates, and final removal of workspace members.

## 18. Acceptance Criteria

The rewrite is complete only when all of the following are true:

1. No LLM-enabled binary can directly place an order, resume trading, increase risk, or load a model without a validated deployment workflow.
2. One persisted mission can pause and resume without repeating completed iterations.
3. One Agent-created data mission produces a quality report and content-addressed dataset manifest through existing Rust connectors.
4. One real LLM call produces a hypothesis artifact through bounded research tools.
5. Real GP, MCTS, and Bayesian engines produce reproducible candidate lineage and budget evidence.
6. One offline RL engine consumes persisted traces and produces a lab-only proposal without live authority.
7. Learning evidence changes a versioned next-run search policy and records whether the change improved validation results.
8. DuckDB persists complete lineage from dataset to candidate to evaluation to registry revision.
9. Sealed holdout data cannot be read by active search-engine tools.
10. A promoted factor or model reaches paper and shadow through `apps/live` without research code importing an execution adapter.
11. A forged, expired, replayed, signature-invalid, or payload-mismatched deployment envelope is rejected.
12. Runtime attribution and rollback events create later research memory without modifying live code.
13. Legacy Agno control-plane, unsafe Ops Agent, static prototype wrappers, and obsolete deployments are absent from active build and deployment paths.
14. Documentation and CLI status accurately distinguish implemented, simulated, deferred, and live-capable functionality.

## 19. Chosen Approach

Use a strangler replacement of the research and control plane around the retained Rust runtime.

Rejected alternatives:

- Whole-repository rewrite: discards working exchange, risk, OMS, and runtime assets while increasing execution risk.
- Continued patching of the current skeleton: preserves misleading boundaries and makes cleanup harder.
- Immediate distributed event-sourced microservices: adds operational cost before a single-process research kernel is proven.

## 20. Implementation Gate

No legacy deletion or production code migration starts until this written spec is reviewed. After approval, create a separate implementation plan with atomic commits, explicit prerequisites for every deletion, and focused validation per phase.
