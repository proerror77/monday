# Agentic Alpha Harness Architecture

This document supersedes the old Python Agno control-plane architecture. Python remains a prototype/training layer only; it is not the primary control plane.

## Boundary

Hot runtime crates keep market data, inference, risk, Sentinel, and execution isolated from agentic research. Agentic systems can propose, evaluate, audit, and request gated rollout. They do not call execution gateway methods directly.

```text
data / replay artifacts
  -> proposal engines: GP, QD, MCTS, RL, LLM, Bayesian, Python prototypes
  -> proposal artifacts
  -> deterministic replay/evaluation
  -> Factor Bank
  -> promotion gate
  -> allocator policy validation
  -> live-small supervisor
  -> audit trail
  -> research memory
  -> learning directive for next loop
```

## Rust Crates

- `research-core/manifest`: reproducible data, feature, label, search, evaluation, promotion, live rollout, and harness manifests.
- `research-core/factor-dsl`: canonical factor formula/program AST.
- `research-core/search-protocol`: proposal artifacts and MCTS trace contracts.
- `research-core/search-protocol`: budgeted MCTS/RL/LLM lab search runs.
- `research-core/factor-bank`: factor assets, lineage, status, and MVP execution guardrails.
- `research-core/factor-eval`: deterministic gates and local replay CSV metrics.
- `research-core/promotion-gate`: paper, shadow, and live-small gates.
- `research-core/live-small-supervisor`: live-small allow/block/rollback decisions.
- `research-core/prototype-adapter`: lab-only wrappers around existing Python prototypes.
- `research-core/research-memory`: structured failures and learning directives.
- `research-core/loop-engine`: loop state, stage status, and stop decisions.
- `research-core/allocator-policy`: proposed weights checked against hard caps.
- `research-core/audit-trail`: validated end-to-end audit bundles.
- `infra-services/core/*-store`: typed in-memory and file-backed cold-path stores.
- `apps/agentic-alpha`: local operator CLI for harness readback.

## Python Prototype Boundary

Current Python prototypes are wrapped as lab-only backends:

- LOB alpha search.
- OHLCV alpha generator.
- BBO optimizer.
- RL lab generator.
- Signal aggregator.
- Smart exit manager.

They may generate proposal artifacts. They may not write directly to Factor Bank, mutate live weights, or trade.

`python-retirement-demo` reports the current replacement queue. The local harness no longer requires Python execution; existing Python files remain parity references until production engines replace them.

## Current Local Loop

Implemented local commands:

```bash
cargo run -p hft-agentic-alpha -- topology
cargo run -p hft-agentic-alpha -- engine-loop-demo target/agentic-alpha/engine-loop.json
cargo run -p hft-agentic-alpha -- prototype-lab-demo target/agentic-alpha/prototype-lab.json
cargo run -p hft-agentic-alpha -- replay-eval target/agentic-alpha/replay.csv target/agentic-alpha/replay-factors.json target/agentic-alpha/replay-report.json
cargo run -p hft-agentic-alpha -- learning-demo target/agentic-alpha/learning.json
cargo run -p hft-agentic-alpha -- live-command-demo target/agentic-alpha/live-command.json
cargo run -p hft-agentic-alpha -- live-command-demo target/agentic-alpha/armed-live-command.json --approval-ref approval-demo-1
cargo run -p hft-agentic-alpha -- connectivity-smoke target/agentic-alpha/connectivity-smoke.json --evm-rpc-url https://ethereum-rpc.publicnode.com
cargo run -p hft-agentic-alpha -- python-retirement-demo
cargo run -p hft-agentic-alpha -- export-audit target/agentic-alpha/audit-demo.json
```

## Still Deferred

- Real ClickHouse-backed research stores.
- Real full-domain data manifests.
- Production MCTS/RL/LLM engines with real model/tool execution.
- Binding approved live-small runtime commands to credentialed production order adapters.
- Broad Python removal.
