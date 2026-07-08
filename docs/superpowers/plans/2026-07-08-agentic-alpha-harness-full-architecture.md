# Agentic Alpha Harness Full Architecture Plan

Date: 2026-07-08
Status: Active implementation plan

## Goal

Move beyond Phase 1 contracts into a complete Rust-first architecture skeleton for the research-to-trading loop.

This plan does not make autonomous live trading production-ready. It makes the architecture real in code: evaluation contracts, storage ports, loop orchestration contracts, replay/control entry points, and targeted validation lanes.

## Non-Negotiables

- Keep agentic research outside hot runtime crates.
- All proposal engines feed deterministic validation before promotion.
- Live-small sizing stays policy input, not a hardcoded product constant.
- Use Rust for durable boundaries.
- Keep Python as a prototype adapter layer only.
- Validate changed crates directly; do not compile the whole workspace per edit.

## Architecture Deliverables

### L1 Research Harness

- `research-core/factor-eval`: point-in-time evaluation and leakage gate contracts.
- Existing `factor-dsl`, `search-protocol`, `factor-bank`, `manifest`.

### L1/L2 Storage Ports

- `infra-services/core/artifact-store`: immutable manifest/artifact access ports.
- `infra-services/core/experiment-store`: search/evaluation run index ports.
- `infra-services/core/factor-store`: Factor Bank repository ports.

These are traits and records first, not ClickHouse implementations. SQL wiring belongs in a later narrow spike.

### L2 Trading Harness

- Existing `promotion-gate` remains the deterministic gate.
- Gate now consumes validated Factor Bank assets.

### Loop Orchestration

- `apps/agentic-alpha`: prints and validates loop topology for research, event-triggered, decay, trading, and harness-improvement loops.

### Operator Tools

- `tools/factorctl`: local factor validation/readback helper.
- `tools/harnessctl`: local harness loop/topology readback helper.

## Targeted Validation

```bash
cd rust_hft
cargo test -p hft-factor-eval -p hft-artifact-store -p hft-experiment-store -p hft-factor-store --locked
cargo check -p hft-agentic-alpha -p hft-factorctl -p hft-harnessctl --locked
```

## Completion Definition

- All deliverables exist as workspace members, not default members.
- No Python or hot runtime crate changes.
- Storage crates expose typed ports, not raw SQL.
- Agentic app depends on contracts and stores, not execution gateway.
- Targeted validation passes.
