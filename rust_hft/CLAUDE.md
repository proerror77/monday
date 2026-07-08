# Agentic Alpha Harness Status

**Status**: Rust-first agentic alpha harness skeleton is implemented. It is not a full autonomous live-trading system.

## Current Direction

The project direction is no longer a Python control-plane HFT system. The durable architecture is:

```text
prototype engines / agents
  -> proposal artifacts
  -> replay/evaluation gates
  -> Factor Bank
  -> promotion gates
  -> live-small supervision
  -> audit artifacts
  -> research memory / learning directives
```

Agentic research stays outside hot runtime crates. LLM, RL, MCTS, GP, QD, Bayesian, and Python prototype outputs must enter through proposal artifacts and deterministic gates.

## Implemented Harness Boundaries

- `research-core/manifest`: reproducible manifests.
- `research-core/factor-dsl`: canonical factor AST.
- `research-core/search-protocol`: proposal artifacts and MCTS trace validation.
- `research-core/factor-bank`: auditable factor assets and MVP execution status rules.
- `research-core/factor-eval`: deterministic metric gates plus local replay CSV evaluation.
- `research-core/promotion-gate`: paper/shadow/live-small promotion checks.
- `research-core/live-small-supervisor`: live-small rollout and rollback decisions.
- `research-core/prototype-adapter`: lab-only wrappers for Python/RL/BBO/signal/exit prototypes.
- `research-core/research-memory`: structured failures and learning directives.
- `research-core/loop-engine`: turn/goal/time/event loop state and stop decisions.
- `research-core/search-protocol`: budgeted lab search runs for MCTS/RL/LLM proposal generation.
- `research-core/live-small-supervisor`: dry-run and approval-gated non-dry-run runtime command boundary for live-small staging/rollback.
- `research-core/live-small-supervisor`: runtime actuation result contracts for exchange/on-chain connectors.
- `research-core/allocator-policy`: proposed allocation weights checked against hard caps.
- `research-core/audit-trail`: validated harness audit bundles.
- `infra-services/core/{artifact-store,experiment-store,factor-store}`: typed in-memory and file-backed stores.
- `apps/agentic-alpha`: local CLI readback for topology, prototypes, replay evaluation, learning, audit, stores, connectivity smoke, and approval-gated Binance order binding.

## Not Implemented Yet

- Real ClickHouse-backed research stores.
- Real full-domain data wiring into manifests.
- Production MCTS/RL/LLM engines with real model/tool execution.
- Binding approved live-small runtime commands to non-Binance production order adapters.
- Full Python retirement.

## Validation Rule

Do not compile the whole workspace for ordinary harness changes. Use focused lanes:

```bash
cargo test -p hft-factor-eval --locked
cargo test -p hft-prototype-adapter --locked
cargo test -p hft-research-memory --locked
cargo check -p hft-agentic-alpha -p hft-factorctl -p hft-harnessctl --locked
```

For broader harness changes:

```bash
cargo test -p hft-research-manifest -p hft-factor-dsl -p hft-search-protocol -p hft-factor-bank -p hft-promotion-gate -p hft-factor-eval -p hft-prototype-adapter -p hft-live-small-supervisor -p hft-loop-engine -p hft-research-memory -p hft-allocator-policy -p hft-audit-trail -p hft-artifact-store -p hft-experiment-store -p hft-factor-store --locked
cargo check -p hft-agentic-alpha -p hft-factorctl -p hft-harnessctl --locked
```
