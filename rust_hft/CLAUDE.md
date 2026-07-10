# Rust Workspace Instructions
## Current System

This workspace has two trust domains:

- `alpha-harness/*` is the cold Agentic Alpha research/control plane.
- `apps/live`, runtime, risk, OMS, and execution adapters own actuation.

Never add execution-adapter dependencies or order commands to `alpha-harness`. LLM/RL/MCTS/GP outputs are candidate artifacts, not executable trading instructions.

## Durable Packages

- `alpha-domain`: mission, candidate, learning, and deployment contracts.
- `alpha-store`: DuckDB control-plane source of truth.
- `alpha-engine`: search, evaluation, failure critic, and learning loop.
- `alpha-harness`: Agent-facing structured CLI.
- `hft-live`: signed runtime handoff and execution runtime.
- `hft-collector`: connector-owned data acquisition.
- `hft-factor-dsl`, `hft-factor-bank`, `hft-research-manifest`, and `hft-search-protocol` remain shared contracts with active consumers.

## Validation Lanes

Do not compile the full workspace for ordinary work.

```bash
cargo test -p alpha-domain --locked
cargo test -p alpha-store --locked
cargo test -p alpha-engine --locked
cargo test -p alpha-harness --locked
cargo test -p hft-live --no-default-features --test deployment_envelope --locked
cargo check -p hft-collector --locked
```

Run `cargo metadata --locked --no-deps` after package additions or removals. Use release/full graph checks only in release work.

## Data Integrity

- No silent real-to-fixture fallback.
- Verify content hashes before reading datasets.
- Preserve point-in-time availability and sealed holdout boundaries.
- Keep iterations, feedback, approvals, policies, and deployment evidence append-only.
- Never persist LLM API keys or private signing keys.
- Live-small stays disabled until envelope limits are enforced in every order path.
