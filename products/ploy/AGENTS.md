# PLOY Product Instructions

This directory is the PLOY product workspace inside the Monday monorepo.

## Authority boundary

- Follow the repository-root `CLAUDE.md` and `docs/architecture/PLOY_INTEGRATION.md`.
- Monday `rust_hft` is the only production authority for risk, OMS, reconciliation, cancellation, and order execution.
- PLOY product, prediction-market, research, frontend, sidecar, and control-plane code may not bypass that authority.
- Keep live trading disabled unless a separate reviewed task explicitly rebuilds and approves the required Monday gates.

## Development workflow

- Run Rust commands from `products/ploy`; this is an independent Cargo workspace pinned by `rust-toolchain.toml`.
- Run frontend commands with `npm --prefix ploy-frontend` and sidecar commands with `npm --prefix ploy-sidecar`.
- Use `tasks/todo.md` for non-trivial work and keep changes atomic.
- Use `apply_patch` for manual edits, preserve unrelated changes, and verify with focused checks before the full PLOY CI lane.
- Do not run a local PostgreSQL instance. Database-backed validation belongs in GitHub Actions.

## Legacy material

The nested `.github`, `deployment`, and `infra` directories are retained for historical contracts and source compatibility. They are not active Monday workflows or approved deployment entrypoints. Do not copy them to the repository-root `.github/workflows` without an explicit architecture and security review.
