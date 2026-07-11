# Control-Plane Contract Index

This file is an index, not an independent protocol definition. Rust types and validators are the source of truth.

## Research Contracts

- `rust_hft/alpha-harness/domain`: `ResearchMission`, `LoopRun`, `CandidateArtifact`, evaluation evidence, approvals, promotion records, strategy bundles, deployment envelopes, and runtime attribution.
- `rust_hft/alpha-harness/store`: immutable revisions, append-only iterations, authenticated checkpoints, approvals, policy memory, and deployment evidence.
- `rust_hft/tools/collector`: governed dataset manifests, source capabilities, time bounds, and quality reports.

## Runtime Boundary

The only research-to-runtime activation path is a signed `DeploymentEnvelope` bound to a persisted promotion and exact `StrategyBundle`. Runtime code revalidates hashes, policy scope, account, venue, instruments, limits, validity, signature, nonce, and runtime-owned approval evidence. Runtime feedback is separately signed and must be verified before research ingestion.

The research plane does not call `StartTrading`, `LoadModel`, order, cancel, wallet, or transaction endpoints. Runtime pause, resume, artifact load, strategy replacement, and risk increase remain runtime/governance operations and cannot be inferred from an LLM message or Redis event.

Paper and simulated-execution Shadow Formula activation are implemented. Live-small activation is disabled. ONNX remains runtime compatibility only, accepts only contained bundle-relative files, and has no governed v2 promotion producer.

## Source References

- [Canonical architecture](../rust_hft/ARCHITECTURE.md)
- [Alpha Harness CLI](../rust_hft/alpha-harness/README.md)
- [Runtime event contracts](../rust_hft/docs/contracts.md)
- [Approved production-hardening design](superpowers/specs/2026-07-11-loop-engineer-production-hardening-design.md)
