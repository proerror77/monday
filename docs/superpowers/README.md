# Design Status

## Current Source Of Truth

- [Repository authority and delivery policy](../../AGENTS.md)
- [Current architecture](../../rust_hft/ARCHITECTURE.md) and
  [module ownership](../architecture/REPOSITORY_LAYOUT.md)
- [Task and symptom navigation](../agents/scenarios.md)
- [CEX Campaign entrypoints](../../rust_hft/alpha-harness/README.md#cex-campaign)
  and [cloud execution/evidence contracts](../../deployment/aliyun/research/README.md)

Use the owning implementation and its current tests to resolve behavior. A
dated design or completed plan records intent and history; it does not establish
current deployment, research results, or runtime authority.

## Historical Loop Engineer Design

The [2026-07-11 production-hardening design](specs/2026-07-11-loop-engineer-production-hardening-design.md)
and [implementation plan](plans/2026-07-11-loop-engineer-production-hardening.md)
record the legacy bounded LoopRun, Formula/ONNX and signed Paper/Shadow work.
Their whole-system capability claims predate the canonical CEX Campaign and
closed-family supervised evaluation. Consult the current contracts above before
using an entrypoint or making a completion claim.

## Superseded Documents

The 2026-07-08 Agentic Alpha MVP/contracts/full-architecture documents and the 2026-07-10 v2 design/plan are retained only as implementation history. They contain earlier capability claims and must not be used as production or runtime authority.

Git history is the recovery source for removed completion reports and obsolete Agno/OMX documentation.
