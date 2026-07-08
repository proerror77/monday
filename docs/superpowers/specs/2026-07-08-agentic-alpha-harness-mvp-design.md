# Agentic Alpha Harness MVP Design

Date: 2026-07-08
Status: Approved design draft

## Summary

Redesign the existing HFT project into an Agentic Alpha Harness: a research-to-trade system that uses full-domain event data, multiple search engines, machine learning, reinforcement learning, LLM agents, and deterministic promotion gates to discover, validate, trade, monitor, and improve crypto alpha strategies.

This is not a rewrite. The existing Rust HFT runtime, collectors, backtest app, risk layer, ML workspace prototypes, LOB alpha search, OHLCV alpha generator, BBO module, RL stub, signal aggregator, smart exit manager, and ClickHouse paths are treated as prototypes or foundations to reorganize.

The main shift is from:

```text
HFT execution platform + ML training workspace + ops monitor
```

to:

```text
Agentic Alpha Harness
  Research Loop
  Trading Loop
  Harness Self-Improvement Loop
```

## Hard Constraints

1. Use Rust wherever it should be the durable system boundary.
2. Reduce Python usage over time.
3. Keep Python for existing prototypes, notebooks, quick experiments, and optional model-training adapters until Rust equivalents exist.
4. Do not put agentic research logic directly in the Rust hot execution path.
5. Do not let LLM, RL, MCTS, GP, or Bayesian search bypass deterministic validation and risk gates.
6. Do not hardcode live-small risk percentages as product constants. They are policy values proposed by allocation models or agents, then validated against system hard caps.
7. Do not validate every development change by compiling the whole Rust workspace. Use targeted validation lanes.
8. Do not use OMX/autoresearch workflow strategy for this project direction.

## Product Boundary

The MVP is an end-to-end, live-small capable Agentic Alpha Harness.

It supports:

- Full-domain research across LOB, tick trades, OHLCV, OI, funding, liquidation, cross-exchange, on-chain, listing, event, sentiment, and ICI-like external signals.
- Tick-first event alignment with point-in-time availability.
- Hybrid output: factor scores plus regime state plus validated trade intent.
- Full multi-engine proposal generation: GP, Quality-Diversity, MCTS, RL generator, Bandit/RL allocator, LLM proposer, and Bayesian optimizer.
- Manifest-backed reproducibility for data, features, labels, search runs, evaluations, promotions, and live rollouts.
- Live-small trading with first same-class human approval and later gated auto-promotion.
- A harness self-improvement loop that can improve workflows, prompts, search spaces, evaluators, and memory retrieval only after regression validation.

It does not support:

- Unbounded autonomous live trading.
- Direct LLM edits to live weights without artifacts and gate approval.
- Direct RL policy control of real accounts without promotion gates.
- Full reliability for every full-market data source on day one.
- Rewriting the Rust execution runtime as part of the MVP.

## Loop Architecture

### Research Loop

```text
manifested data
  -> hypothesis
  -> GP / QD / MCTS / RL / LLM / Bayesian search
  -> evaluator
  -> Factor Bank
  -> Research Memory
```

The research loop discovers and improves factor assets. It can propose formula factors, program factors, model features, model configs, factor ensembles, and allocator policies. It does not execute trades.

### Trading Loop

```text
factor scores + regime state
  -> allocator / policy proposal
  -> promotion gate
  -> live-small rollout
  -> attribution / decay / rollback
  -> memory
```

The trading loop turns approved research assets into bounded trade intents. The Rust runtime executes only validated, signed, manifest-referenced policies.

### Harness Self-Improvement Loop

```text
failure cluster
  -> bounded harness change proposal
  -> held-in validation
  -> held-out regression
  -> accept or reject
  -> harness version bump
```

The harness loop improves how agents work. It can modify prompt templates, search spaces, workflow ordering, feature sets, evaluation recipes, gate thresholds inside allowed policy bounds, and memory retrieval strategy. It cannot weaken hard risk caps, bypass execution gates, modify secret handling, or create a new live permission class without human approval.

## System Layers

```text
L0 Data Plane
  collectors, raw event artifacts, ClickHouse indexes, manifests

L1 Research Harness
  factor factory, search engines, evaluator, Factor Bank, Research Memory

L2 Trading Harness
  allocator, regime gate, promotion gate, policy store, live-small supervisor

L3 Rust Runtime
  market data runtime, inference, risk-control, Sentinel, execution gateway
```

### L0 Data Plane

The data plane is the truth surface. Every event must be represented with point-in-time semantics.

Required fields:

```text
event_time
exchange_time
receive_time
available_time
ingestion_time
source
source_latency_ms
symbol or event_id
schema_version
quality_flags
```

`available_time` is mandatory for anything used in backtesting, labeling, research, or live decisions.

### L1 Research Harness

The research harness runs candidate generation, search, evaluation, and memory distillation.

It owns:

- Factor DSL and program-factor AST.
- Proposal artifacts.
- Search traces.
- Evaluation manifests.
- Factor Bank writes.
- Research memory writes.

### L2 Trading Harness

The trading harness decides whether research outputs can affect live-small.

It owns:

- Allocator policy proposals.
- Risk policy proposals.
- Promotion manifests.
- Approval state.
- Auto-promotion class state.
- Live-small rollout supervision.
- Rollback triggers.

### L3 Rust Runtime

The Rust runtime remains the controlled execution layer.

It owns:

- Market data ingestion and runtime state.
- Low-latency scoring and inference where needed.
- Risk-control and Sentinel.
- Order intent lifecycle.
- Execution gateway.
- Feedback export.

Agents do not directly call execution gateway methods.

## Rust-First Project Structure

Durable boundaries should be Rust crates. Python remains a compatibility and experimentation layer.

Proposed Rust additions:

```text
rust_hft/research-core/
  factor-dsl/
  factor-bank/
  factor-eval/
  manifest/
  search-protocol/
  promotion-gate/

rust_hft/infra-services/core/
  factor-store/
  experiment-store/
  artifact-store/

rust_hft/apps/
  agentic-alpha/
  factor-replay/

rust_hft/tools/
  factorctl/
  harnessctl/
```

Responsibilities:

- `factor-dsl`: canonical AST, operators, type system, serialization, and deterministic evaluation contracts.
- `manifest`: typed manifests for data, features, labels, search, evaluation, promotion, and live rollouts.
- `factor-bank`: factor definitions, versions, lineage, metrics, status, and decay state.
- `factor-eval`: IC, RankIC, ICIR, PnL, drawdown, turnover, correlation, cost-aware metrics, leakage checks, and walk-forward split contracts.
- `search-protocol`: shared proposal artifact schema for GP, QD, MCTS, RL, LLM, and Bayesian engines.
- `promotion-gate`: deterministic gate evaluation before paper, shadow, live-small, or rollback decisions.
- `factor-store`: ClickHouse-backed storage for Factor Bank and metrics.
- `experiment-store`: search run and evaluator artifact indexes.
- `artifact-store`: manifest-indexed access to immutable trace and Parquet artifacts.
- `agentic-alpha`: orchestrates the MVP loops without joining the hot execution path.
- `factor-replay`: targeted replay/evaluation runner for factor and policy validation.
- `factorctl` and `harnessctl`: operator and CI-friendly commands for local checks.

Python compatibility layer:

```text
ml_workspace/
  factor_factory/
  harness/
  adapters/
```

Python should wrap existing prototypes:

- `lob_core/alpha_search.py`
- `algorithms/alpha/true_alpha_generator.py`
- `algorithms/bbo/search.py`
- `algorithms/rl/trainer.py`
- `algorithms/signal_aggregator.py`
- `algorithms/smart_exit_manager.py`

The long-term direction is to move stable contracts and evaluators into Rust, while leaving heavyweight model training and ad hoc experiments in Python.

## Rust Library Replacement Strategy

The project has existing Python research prototypes. They should not all be preserved as permanent architecture. Replace Python modules when the behavior is stable enough to deserve a durable Rust contract, and only after a crate-level spike proves compile cost, ergonomics, and runtime behavior are acceptable.

Candidate Rust ecosystem choices as of this design:

```text
tabular feature engineering:
  Polars Rust for local DataFrame-style lazy/eager feature work and Parquet scans
  DataFusion for Arrow-native query execution, SQL-like feature views, and custom query systems

columnar interchange:
  Apache Arrow / Parquet as the shared memory and artifact format

model inference:
  ort for ONNX Runtime integration where exported models remain the deployment contract

Rust-native ML:
  Burn for Rust-native tensor/deep-learning experiments that need training + inference in Rust
  Candle for lightweight Rust-native model inference/training experiments, especially Hugging Face style models

optimization:
  argmin for numerical optimization and some Bayesian/parameter-search building blocks

serialization:
  serde JSON for readable manifests first
  compact binary encoding only after schema stability and profiling
```

Selection rules:

1. Prefer Rust crates already compatible with the workspace's compile-time and feature-gate discipline.
2. Use feature flags to keep heavy ML/data dependencies out of hot-path crates.
3. Put experimental dependencies behind isolated crates, not broad workspace dependencies.
4. Keep ONNX inference as the first production bridge where model training still happens outside Rust.
5. Move Rust-native ML to production only after parity tests against the current Python model flow.
6. Do not choose a crate because it is new. Choose it only when it reduces operational complexity or removes a real Python bottleneck.

### Python Retirement Order

Replace Python in this order:

```text
1. schemas and manifests
2. Factor Bank records and storage access
3. deterministic DSL / AST evaluation
4. leakage, split, and promotion gates
5. feature engineering kernels that are reused in live or replay
6. replay/evaluation runners
7. parameter optimization loops
8. model inference
9. model training where Rust-native ML is proven
10. exploratory notebooks and one-off research last, or never
```

Python modules that stay temporarily:

- Existing `ml_workspace/lob_core` alpha search while it is being wrapped.
- Existing BBO and RL stubs until Rust contracts exist.
- Heavy model training experiments where PyTorch ecosystem speed matters.
- Ad hoc exploratory research.

Python modules that should become Rust contracts first:

- Manifest schemas.
- Factor DSL AST.
- Factor Bank status and lineage.
- Promotion gate evaluation.
- Point-in-time availability checks.
- Reproducible replay and evaluator contracts.

### Replacement Spike Policy

Each Python-to-Rust replacement starts with a narrow spike:

```text
crate: isolated experimental crate or feature-gated module
input: fixed fixture or manifest
output: parity artifact against existing Python behavior
validation: targeted cargo check/test for that crate only
decision: adopt, revise, or reject
```

No replacement is accepted without:

- Fixture parity where Python behavior exists.
- Compile-time impact noted.
- Feature flags documented.
- A targeted validation command.
- A rollback path to the previous Python adapter.

## Data and Manifest Design

Use Hybrid with manifest.

```text
Raw artifacts:
  Parquet, trace files, source snapshots

Queryable layer:
  ClickHouse for aligned events, features, metrics, and Factor Bank indexes

Manifest:
  every dataset, evaluation, model, factor, promotion, and live rollout references exact versions
```

### Data Domains

MVP data domains:

```text
market_microstructure:
  LOB trace, tick trades, OHLCV, spread, depth, OFI, CVD

derivatives:
  open_interest, funding, basis, long_short_ratio, liquidation

cross_exchange:
  venue depth, venue spread, basis dispersion, OI share, price dislocation

on_chain:
  exchange inflow, exchange outflow, whale transfer, stablecoin flow

event_signals:
  listing, delisting, news, social, sentiment, ICI-like external signals
```

### Manifest Types

Required MVP manifests:

```text
data_manifest:
  source, symbols, time_range, artifact_paths, schema_versions, quality summary

feature_manifest:
  feature_set_id, operators, windows, normalization, availability policy

label_manifest:
  horizon, barrier config, fees, slippage, funding cost, label version

search_manifest:
  engine, seed, prompt/model version, search space, parent run ids

evaluation_manifest:
  dataset version, evaluator version, metrics, costs, walk-forward split

promotion_manifest:
  factor/model/policy id, gate results, approval mode, rollout limits

live_rollout_manifest:
  runtime config, risk policy, start/end, attribution, rollback result

harness_manifest:
  harness version, agents, prompts, tool permissions, evaluator versions, memory snapshot
```

Only manifest-backed outputs can be promoted.

## Factor Bank and Candidate Pool

The Candidate Pool is wide and permissive. The Factor Bank is strict and auditable.

### Candidate Pool

Candidate Pool inputs:

- GP mutations.
- QD novelty candidates.
- MCTS tree expansions.
- RL-generated factor collections.
- LLM-generated hypotheses, DSL, and program factors.
- Bayesian parameter variants.
- Manual seed factors.

Candidates are stored as proposal artifacts, not as deployable assets.

### Factor Bank

Every Factor Bank asset must include:

```text
factor_id
factor_type: formula | program | model_feature | model | ensemble | allocator_policy
dsl_or_program_ast
source_engine
parent_factor_ids
data_manifest_id
feature_manifest_id
label_manifest_id
search_manifest_id
evaluation_manifest_ids
metrics
correlation_cluster
regime_metrics
symbol_metrics
promotion_status
live_decay_state
created_at
updated_at
```

Promotion statuses:

```text
generated
quick_test_passed
full_backtest_passed
paper_trading
live_shadow
live_small_pending_approval
live_small
live_full_candidate
decayed
retired
rejected
```

`live_full_candidate` is a bookkeeping state only in the MVP. The MVP does not auto-promote to full live trading.

## Search Engines

The MVP includes all engines but separates their rights.

### GP / Evolution

Purpose:

- High-volume formula and program mutation.
- Crossovers.
- Simple explainable factors.

Output:

- Proposal artifacts.
- Factor AST candidates.

### Quality-Diversity

Purpose:

- Avoid redundant OI/funding/price variants.
- Reward novelty across data source, time horizon, regime, and correlation cluster.

Reward shape:

```text
score = performance
  + novelty_bonus
  - correlation_penalty
  - complexity_penalty
  - instability_penalty
```

### MCTS

Purpose:

- Search DSL/program factor trees.
- Explore structured conditional logic.

Node model:

```text
node = partial or complete factor AST/program
action = add operator, mutate feature, change window, add condition, simplify subtree
selection = UCB or PUCT variant
expansion = deterministic mutation or LLM-assisted proposal
evaluation = evaluator artifact
backpropagation = update ancestors through real parent lineage
```

MCTS must include parent links, visits, total reward, best reward, cycle guard, and visible truncation metrics. A tree without real lineage and backpropagation is not accepted as MCTS.

### LLM Agents

Purpose:

- Generate hypotheses.
- Explain failures.
- Propose DSL/program candidates.
- Propose model and allocator configs.
- Summarize research memory.

LLM agents do not decide profitability. Evaluators do.

### Bayesian Optimization

Purpose:

- Tune windows, thresholds, barriers, holding time, stops, fee/slippage assumptions, and model hyperparameters.

### RL Generator

Purpose:

- Generate formulaic alpha collections.
- Learn search policy in lab.

MVP rights:

- Lab proposal only until enough Factor Bank history exists.

### Bandit / RL Allocator

Purpose:

- Propose factor weights, regime routing, and bounded risk policy.

MVP rights:

- Can influence live-small only through promotion gate.

## Agent Roles and Loops

Roles:

```text
HypothesisAgent
GeneratorAgent
SearchAgent
EvaluatorAgent
ValidatorAgent
FactorBankAgent
AllocatorAgent
RiskGateAgent
DecayMonitorAgent
MemoryAgent
HarnessCriticAgent
```

First workflows:

```text
daily_research_loop:
  mine factors, evaluate, update Factor Bank and memory

event_triggered_opportunity_loop:
  react to listing, liquidation, OI/funding shock, on-chain shock, or data anomaly

live_decay_loop:
  monitor live/shadow decay, attribution, slippage, data quality, rollback triggers

harness_improvement_loop:
  mine repeated failures and propose bounded harness changes
```

Every loop declares:

```text
trigger
input manifests
allowed tools
permission level
output artifacts
evaluator
stop condition
rollback behavior
```

## Trading Loop and Live-Small

Output is hybrid:

```text
factor_scores
regime_state
allocator_policy
risk_policy
validated_trade_intent
```

Live-small promotion flow:

```text
proposal
  -> deterministic validation
  -> replay / walk-forward
  -> paper or shadow evidence
  -> risk policy gate
  -> first same-class human approval
  -> bounded live-small rollout
  -> attribution
  -> automatic rollback if gates fail
```

Promotion mode:

```text
first_approval_required = true
same_class_auto_promotion_after_first_approval = true
auto_promotion_requires_all_gates = true
auto_rollback_required = true
```

Research universe:

```text
all symbols and events with available data
```

Tradable universe:

```text
only symbols/events passing liquidity, spread, depth, venue reliability, data freshness, and risk gates
```

Risk policy is configurable and proposed by allocator/model/agent. It must be validated against hard caps owned by the runtime and risk gate:

```text
global account loss cap
per-symbol exposure cap
per-factor weight cap
venue liquidity cap
spread/depth cap
data freshness cap
latency cap
drawdown cap
data-gap cap
Sentinel emergency stop
```

## Error Handling and Safety

Failure classes:

```text
data_unavailable
data_stale
manifest_missing
schema_mismatch
leakage_detected
insufficient_sample
overfit_detected
high_correlation
gate_failed
approval_required
risk_cap_exceeded
runtime_rejected
sentinel_stopped
rollback_failed
```

Rules:

1. Missing manifests block promotion.
2. Missing `available_time` blocks evaluation and promotion.
3. Data quality failures can still enter research memory but cannot enter Factor Bank as promotion candidates.
4. Gate failures must produce artifacts, not just logs.
5. Runtime rejection must be fed back to Factor Bank and Research Memory.
6. Rollback must be idempotent.
7. Live-small changes must be attributable to a manifest and promotion decision.

## Testing and Validation Strategy

Do not compile or test the whole Rust workspace for every change. Use targeted lanes.

### Validation Lanes

Documentation-only:

```text
markdownlint or local markdown checks if available
git diff --check
```

Rust schema and contract crate changes:

```text
cd rust_hft
cargo check -p <changed-crate> --locked
cargo test -p <changed-crate> <focused-test> --locked
```

Rust dependency boundary changes:

```text
cd rust_hft
cargo check -p <changed-crate> --locked
cargo tree -p <changed-crate> -e features
```

Runtime integration changes:

```text
cd rust_hft
cargo check -p hft-live --locked
cargo test -p hft-engine <focused-test> --locked
```

Collector/data adapter changes:

```text
cd rust_hft
cargo check -p hft-collector --locked
targeted adapter smoke or schema validation script
```

Python compatibility changes:

```text
targeted pytest for changed module
no broad ml_workspace test sweep unless interface contracts changed
```

Promotion gate changes:

```text
unit tests for gate pass/fail cases
manifest fixture regression
held-in and held-out evaluator fixtures
```

Full workspace validation is reserved for:

- Release candidates.
- Dependency graph changes that affect many crates.
- Cross-layer runtime integration.
- Before merging a broad architecture branch.
- Explicit user request.

### Crate Structure Rules

1. Prefer small crates with one purpose over large cross-layer crates.
2. Avoid cyclic dependencies.
3. `research-core` crates must not depend on `apps`.
4. Hot runtime crates must not depend on agent orchestration crates.
5. Storage crates expose typed repositories, not raw SQL everywhere.
6. Manifest and Factor Bank schemas are shared contracts.
7. Feature flags must be scenario-based and documented.

## Phasing

### Phase 0: Spec and Contract Stabilization

- Write this design spec.
- Define implementation plan.
- Decide crate boundaries.
- Define manifest and Factor Bank contracts.
- Define targeted validation lanes.

### Phase 1: Rust Contracts and Stores

- Add manifest crate.
- Add factor DSL schema crate.
- Add search proposal schema.
- Add Factor Bank schema and storage interface.
- Add promotion gate contract.
- Add CLI readback tools.

### Phase 2: Wrap Existing Prototypes

- Done locally: wrap existing LOB alpha search as a proposal backend.
- Done locally: wrap OHLCV alpha generator as a proposal backend.
- Done locally: wrap BBO as parameter optimizer backend.
- Done locally: wrap RL stub as lab-only generator.
- Done locally: wrap signal aggregator and smart exit manager as lab-only proposal backends.
- Done locally: store prototype outputs as proposal artifacts in a file-backed experiment log.
- Done locally: expose Python retirement inventory showing the local harness no longer requires Python execution.

### Phase 3: Evaluation and Manifested Replay

- Done locally: add replay CSV evaluator flow over manifested sample columns.
- Add walk-forward and leakage gates.
- Done locally: add replay fixture runner through `hft-agentic-alpha replay-eval`.
- Add factor and policy evaluation manifests.

### Phase 4: Trading Harness and Live-Small Gate

- Add allocator policy schema.
- Done locally: add dry-run and approval-gated non-dry-run live-small runtime command boundary.
- Add risk policy proposal schema.
- Add promotion approval state.
- Add live-small supervisor integration.
- Add feedback exporter into Factor Bank.

### Phase 5: Harness Self-Improvement

- Add failure clustering.
- Add bounded harness change proposals.
- Add held-in and held-out harness regression.
- Add harness versioning.

## Success Metrics

MVP success is a composite score.

Loop health:

- Data to proposal to evaluation to Factor Bank to paper/shadow/live-small to feedback completes.
- Every step has a manifest or artifact.

Research quality:

- OOS stability.
- Low factor correlation.
- Regime robustness.
- Cost-aware net metrics.
- Repeatable search traces.

Trading quality:

- Live-small attribution exists.
- Slippage is measured.
- Drawdown stays within configured policy and hard caps.
- Rollback triggers work.

Harness quality:

- Repeated failures become structured memory.
- Harness changes are validated on held-in and held-out fixtures.
- Accepted harness versions improve future proposals without breaking held-out cases.

## Open Design Decisions for Planning

These are intentionally deferred to the implementation plan:

1. Exact initial Rust crate names.
2. Exact ClickHouse table DDL.
3. Whether Factor DSL AST serialization is JSON, postcard, protobuf, or another stable encoding.
4. The first three data sources to wire into full-domain manifests.
5. The first live-small strategy class requiring human approval.
6. Which existing Python prototype is wrapped first.

## Approval

The user approved this direction with these additions:

- Use Rust as much as possible.
- Reduce Python usage.
- Keep project structure rational.
- Avoid full Rust workspace compile validation on every change.
- Build toward an Agentic Alpha Harness, not just an Alpha Discovery Loop.
