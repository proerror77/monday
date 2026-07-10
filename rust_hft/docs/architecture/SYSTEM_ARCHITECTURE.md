# System Architecture
The canonical architecture is [../../ARCHITECTURE.md](../../ARCHITECTURE.md).

The current system is a Rust execution runtime with a separate Agentic Alpha research/control plane. Agents may acquire governed public data, write hypotheses, generate typed DSL/model candidates, run GP/MCTS/Bayesian/offline-RL/LLM engines, explain failures, and propose promotions. They cannot submit orders.

The only control-plane-to-runtime interface is a signed `DeploymentEnvelope`. Runtime-owned verification checks current account, venue, instruments, config/risk hashes, limits, validity, approval class, signature, and nonce. Paper and shadow activation are implemented. Live-small activation fails closed until all execution paths consume max order-size and slippage limits.

DuckDB stores control-plane state. Trace/Parquet stores research data. ClickHouse remains an optional analytics plane. Runtime nonce, audit, trusted-key, and feedback files remain runtime-owned.
