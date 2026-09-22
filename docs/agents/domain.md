# Domain Docs

Start with the [task map](scenarios.md) and read the owning domain contract for
the requested behavior. Project authority stays in [AGENTS.md](../../AGENTS.md).

Current architecture and ADRs live in:

- [Rust architecture](../../rust_hft/ARCHITECTURE.md) and
  [repository ownership](../architecture/REPOSITORY_LAYOUT.md).
- [System-boundary ADR](../architecture/ADR-0001-monday-v2-system-boundaries.md)
  for Research, Governance, Runtime and market-family ownership.
- [Release-identity ADR](../architecture/ADR-0002-rust-lob-release-identities.md)
  for collector source, artifact and deployed identity.
- [Research contracts](../research/) for Campaign workflows, evaluation,
  holding/accounting and training; use only the files relevant to the task.

Use the vocabulary and invariants in those contracts and their implementation.
Surface a proposed conflict with an applicable ADR. Dated plans and reports
provide historical reasoning or run evidence, not current interface authority.
There is no required root `CONTEXT.md` or `docs/adr/` context tree.
