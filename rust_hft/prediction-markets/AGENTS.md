# Prediction-Market Module Instructions

This directory is Monday's prediction-market research and operator module. PLOY
is a legacy compatibility name for imported crates and binaries, not a separate
product or execution authority.

## Authority boundary

- Follow the repository-root `CLAUDE.md` and `docs/architecture/PREDICTION_MARKETS.md`.
- Monday `rust_hft` is the only production authority for risk, OMS, reconciliation, cancellation, and order execution.
- Prediction-market research, frontend, sidecar, and control-plane code may not bypass that authority.
- Keep live trading disabled unless a separate reviewed task explicitly rebuilds and approves the required Monday gates.

## Development workflow

- Run Rust commands from `rust_hft/prediction-markets`; the nested Cargo workspace is a transitional build seam pinned by `rust-toolchain.toml`.
- Run frontend commands with `npm --prefix ploy-frontend`; run sidecar checks with `cargo test -p ploy-agent-sidecar`.
- Do not create new `ploy-*` crates, a `products/ploy` tree, or another venue execution path. Put new capabilities in the canonical Monday module named in `docs/architecture/PREDICTION_MARKETS.md`.
- Use `tasks/todo.md` for non-trivial work and keep changes atomic.
- Use `apply_patch` for manual edits, preserve unrelated changes, and verify with focused checks before the full PLOY CI lane.
- Do not run a local PostgreSQL instance. Database-backed validation belongs in GitHub Actions.

## Legacy material

Former standalone `deployment` and `infra` material is preserved only under
`docs/archive/standalone-operations`. It is not a Monday deployment entrypoint.
This module has no nested workflow authority and no Python compatibility lane.
Do not restore either tree to the module root without an explicit architecture
and security review.

- When a change supersedes a `ploy-*` identifier, compatibility entrypoint,
  imported contract, or parallel implementation, remove the obsolete path in
  the same change unless a verified cutover must happen first. Cutover-only code
  must additionally name the exact removal scope and the readback condition that
  triggers deletion; do not extend it with new behavior.
