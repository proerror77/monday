# Runtime Contract Index

Rust definitions are the source of truth; this document intentionally does not duplicate struct fields that can drift.

## Market And Execution Events

- Core symbols, prices, quantities, timestamps, and market events: `market-core/core/src` and `market-core/ports/src/events.rs`.
- Strategy intent and execution ports: `market-core/ports/src/traits.rs`.
- OMS state and order transitions: `risk-control/oms-core/src`.
- Runtime construction and venue reconciliation: `market-core/runtime/src/system_builder`.
- Venue execution implementations: `execution-gateway/adapters`.

Required invariants:

- Market sequence gaps invalidate local state and require recovery before trading.
- Strategy output is an intent; risk and execution own order submission.
- Duplicate or out-of-order fills must not double count quantity or PnL.
- Unknown balances, positions, orders, or reconciliation results fail closed.
- Emergency mode rejects new intents and cancels locally open orders.

## Research-To-Runtime Contracts

- Typed missions, candidates, evaluations, bundles, approvals, and signed envelopes: `alpha-harness/domain/src/lib.rs`.
- Runtime envelope intake and nonce/audit protocol: `apps/live/src/deployment_envelope.rs`.
- Signed deployment/strategy-scoped attribution: `apps/live/src/runtime_attribution.rs`.

Only a verified signed envelope plus exact runtime-owned approval evidence may change runtime startup configuration. Redis messages, gRPC requests, LLM output, candidate JSON, and arbitrary model paths are not deployment authority. Feedback JSONL records require a runtime-only Ed25519 signature before the research plane accepts them.

See [the canonical architecture](../ARCHITECTURE.md) for ownership boundaries.
