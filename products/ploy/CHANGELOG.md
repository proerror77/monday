# Changelog

All notable changes to this repository will be documented in this file.

The format is intentionally simple: group changes by release state, then by
impact area. Dates use `YYYY-MM-DD`.

## [Unreleased]

### Runtime and Coordinator

- Extracted coordinator ownership into dedicated capital, governance,
  admission, journal, queue, position, risk, order-intent, and execution
  modules so the live runtime no longer depends on the old platform runtime
  surface.
- Unified managed strategy submit paths through coordinator ingress and removed
  the remaining `SubmitOrder` compatibility path in favor of canonical intent
  submission.
- Reduced bootstrap from a large orchestration module to a thin assembly layer
  with dedicated runtime-spawn, deployment, startup-context, persistence, and
  support submodules.

### Strategy and Runtime Surfaces

- Retired `TradingAgent`, `DomainAgent`, `OrderPlatform`, and other legacy live
  runtime surfaces that previously competed with the canonical strategy runtime.
- Split large live-path owners such as `staggered_arb_live`, `momentum`,
  `strategy_runtime`, `polymarket_ws`, and sidecar handlers into smaller
  modules with clearer ownership boundaries.
- Moved control-plane contracts and runtime-specific types out of `platform`
  and into coordinator, persistence, domain, and RL-specific modules.

### Security and Operations

- Hardened admin authentication cookies from raw SHA-256 fingerprints to
  versioned HMAC-signed cookies.
- Sanitized untrusted AI prompt inputs before they reach Grok/Claude-facing
  prompt builders.
- Locked down deployment-matrix path resolution to reject traversal and unsafe
  roots by default.
- Enforced workflow dependency scanning with `cargo audit` and pinned SSH host
  trust across shell-based and `appleboy/*` deployment workflows.
- Aligned checked-in systemd unit files with restart, memory, and OOM guardrails
  and added regression tests so repo units cannot drift from workflow policy.

### Dependency and Build Hygiene

- Upgraded the direct `rand` dependency to `0.10` and updated all direct RNG
  callsites to the current API surface.
- Removed the last direct `ethers-core` and `ethers-signers` dependency bridge
  from the claimer relayer path by moving ABI encoding, CREATE2 derivation, and
  signing onto `alloy`.
- Added and maintained focused regression coverage for relayer signature
  vectors, governance runtime-state persistence, queue concurrency, and corrupt
  execution-row restore handling.

### Documentation

- Added and expanded implementation tracking in `tasks/todo.md` for the layered
  live-runtime refactor, security hardening waves, and dependency migrations.
