# System Architecture
The canonical architecture is [../../ARCHITECTURE.md](../../ARCHITECTURE.md).

The current system is a deterministic Rust execution runtime with a separate bounded Loop Engineer research/control plane. Agents may acquire governed public data, write hypotheses, generate typed candidates, run GP/MCTS/Bayesian search, perform lab-only offline RL/LLM work, explain failures, and propose promotions. They cannot submit orders.

The only control-plane-to-runtime interface is a signed `DeploymentEnvelope`. Runtime-owned verification checks current account, venue, instruments, config/risk hashes, limits, validity, approval class, signature, and nonce. Paper and shadow activation are implemented. Live-small activation fails closed until all execution paths consume max order-size and slippage limits.

The current governed producer promotes Formula candidates only. ONNX is runtime compatibility, offline RL is lab-only, and neither can acquire promotion authority through the v2 evaluator.

DuckDB stores control-plane state. Trace/Parquet stores research data. ClickHouse remains an optional analytics plane. Runtime nonce, audit, trusted-key, and feedback files remain runtime-owned. External scheduling may invoke a LoopRun but cannot supply missing stage evidence.
