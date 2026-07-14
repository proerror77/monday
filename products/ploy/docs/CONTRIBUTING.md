# Contributing to the PLOY product workspace

PLOY is developed inside Monday at `products/ploy`. Follow the Monday root
`CLAUDE.md`, `products/ploy/AGENTS.md`, and
`docs/architecture/PLOY_INTEGRATION.md` before changing architecture or runtime
boundaries.

## Safety boundary

- Monday `rust_hft` is the only production authority for risk, OMS,
  reconciliation, cancellation, replacement, and execution.
- PLOY live execution remains disabled.
- Do not activate nested PLOY deployment workflows or former standalone host paths.
- Do not add credentials, local runtime state, or generated agent sessions to Git.

## Development setup

Use the toolchains pinned by this workspace: Rust `1.91` and Node `22`. Run commands
from `products/ploy`.

Start with focused checks:

```bash
cargo +1.91 metadata --locked --no-deps --format-version 1
cargo +1.91 fmt --all -- --check
cargo +1.91 test --locked -p ploy-connectivity -p ploy-daemon-host
cargo +1.91 check --locked -p new-ploy-runner --features full
```

Frontend and Rust sidecar examples:

```bash
npm --prefix ploy-frontend ci
npm --prefix ploy-frontend run contracts:check
npm --prefix ploy-frontend run lint
npm --prefix ploy-frontend run build

cargo +1.91 test --locked -p ploy-agent-sidecar
cargo +1.91 clippy --locked -p ploy-agent-sidecar --all-targets --no-deps -- -D warnings
```

Do not start a local PostgreSQL service for routine validation. Database-backed
tests run in the root Monday workflow `.github/workflows/ploy-ci.yml`.

## Change discipline

- Keep commits atomic and use `type(scope): summary` subjects.
- Preserve unrelated changes and use package-scoped checks while iterating.
- Update tests for behavior changes and update operator contracts when schemas change.
- Run `git diff --check` and the relevant product checks before opening a PR.
- Changes that affect execution authority, risk, secrets, or deployment require an
  explicit architecture and security review.

## Pull requests

Open PLOY changes against `proerror77/monday`. The root PLOY CI workflow owns the
current Rust, frontend, schema, audit, and integration lanes. Nested workflows
under `products/ploy/.github/workflows` are historical source material only.
