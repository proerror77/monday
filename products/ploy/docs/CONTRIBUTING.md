# Contributing to Ploy

Guidelines for contributing to the Ploy Polymarket trading bot.

## Development Setup

### Prerequisites

- **Rust** via [rustup](https://rustup.rs/). The repository pins the exact
  toolchain in `rust-toolchain.toml`; run `rustup show active-toolchain` after
  checkout to confirm your shell is using it.
- **PostgreSQL 15+** -- used for event registry, position tracking, and audit logs
- **pkg-config**, **libssl-dev**, **libpq-dev** (Linux) or equivalent (macOS: `brew install postgresql openssl`)
- **Node.js 18+** (only if working on the NBA Swing frontend)

### Clone and Build

```bash
git clone <repo-url> && cd ploy

# Default platform spine build
cargo build -p new-ployd -p new-ploy-runner -p ployctl -p ploytui

# Focused daemon / runner loops
cargo build -p new-ployd
cargo build -p new-ploy-runner  # slim default replay runner
cargo build -p new-ploy-runner --features full  # full live/dry-run runner
```

### Environment

Create a `.env` file (never committed) with at minimum:

```
DATABASE_URL=postgres://ploy:ploy@localhost:5432/ploy
```

Additional variables depend on which domain agents you run (Polymarket API keys, Grok API key, etc.). See the project configuration files for the full list.

## Git Workflow

### Branch Naming

| Prefix      | Purpose                        |
|-------------|--------------------------------|
| `feat/`     | New features                   |
| `fix/`      | Bug fixes                      |
| `refactor/` | Code restructuring (no behavior change) |
| `docs/`     | Documentation only             |
| `test/`     | Adding or updating tests       |
| `chore/`    | Build, CI, dependency updates  |

Example: `feat/kelly-scaling-in`, `fix/circuit-breaker-reset`

### Commit Messages

Follow the **atomic commit** convention -- one commit per logical change.

Format:

```
type(scope): short description
```

**Types**: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`

**Rules**:
- Keep refactors, formatting, and behavior changes in **separate** commits.
- Each commit must build (`cargo build`) and pass relevant tests.
- Avoid WIP commits on shared branches.

Good examples:

```
feat(agents): add ML/ONNX model loading for crypto + RL
fix(circuit_breaker): reset half_open_successes on state transition
refactor(engine): extract slippage protection into separate module
docs: update agent framework design with Phase 1.5 Kelly scaling-in
```

### Pull Request Process

1. Create a feature branch from `main`.
2. Make atomic commits following the conventions above.
3. Push and open a PR with a clear description of the changes.
4. Ensure CI passes (formatting, clippy, build, tests).
5. Request review if the change touches risk management, order execution, or security-sensitive code.

## CI/CD

### Test Pipeline

Every push to `main` and every pull request triggers the **Test** workflow (`.github/workflows/test.yml`), which now validates the current workspace spine directly:

1. **Build** -- package-scoped workspace build for `new-ployd`, `new-ploy-runner`, `ployctl`, `ploytui`, `ploy-daemon-host`, `ploy-runner-host`, and supporting crates
2. **Tests** -- package-scoped workspace tests (against a PostgreSQL 15 service container where needed)

A separate release/deploy path can still build release artifacts, but the default CI lane now covers the shipped runner and the new ownership crates directly.

### Deployment Pipelines

Named host workflows own deployment: `deploy-tango-1-1.yml` for the
research/data/dry-run host and `deploy-trade.yml` for the immutable paused trade
control plane. `approve-live-trade.yml` is the only live resume path and uses a
protected human environment. `release-platform.yml` is build-only portable
artifact verification and cannot mutate a host.

## Running Tests

```bash
# Run the default platform spine
cargo test -p new-ployd -p new-ploy-runner -p ployctl -p ploytui

# Run a specific package
cargo test -p ploy-daemon-host
cargo test -p ploy-runner-host

# Run a specific test
cargo test test_name

# Run a specific package/module slice
cargo test -p ploy-platform-runtime --lib
```

A running PostgreSQL instance is required for integration tests. Set `DATABASE_URL` in your environment or `.env` file.

## Code Style

- Run `cargo fmt` before committing.
- Run `cargo clippy --all-targets` and address warnings.
- Use `thiserror` for library-style errors, `anyhow` sparingly at application boundaries.
- Prefer `rust_decimal::Decimal` over floating-point for monetary values.
- Use `zeroize` for any secret material (private keys, API keys).
- Keep `unsafe` blocks to zero; the codebase currently has none.

## Project Structure

```
apps/
  new-ployd/         -- Next-generation daemon entrypoint
  new-ploy-runner/   -- Next-generation runner entrypoint
  ployctl/           -- Operator CLI
  ploytui/           -- Operator TUI
crates/
  ploy-daemon-host/      -- Daemon host/bootstrap crate
  ploy-runner-host/      -- Runner CLI host crate
  ploy-control-client/   -- Shared operator client transport
  ploy-market-data/      -- Collector/feed/scanner/discovery
  ploy-platform/         -- Control-plane core
  ploy-platform-runtime/ -- Runtime orchestration ownership
  ploy-trading/          -- Canonical trading lifecycle
  ploy-deployments/      -- Worker protocol + supervisor
  ploy-operator-contracts/ -- Shared API/event contracts
  ploy-strategy-runtime/ -- Strategy runtime ownership
  ploy-strategy-bundles/ -- Strategy definitions and composition
  ploy-research/         -- Replay/backtest consumers
```
