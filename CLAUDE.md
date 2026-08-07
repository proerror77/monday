# Repository Instructions

@AGENTS.md

## Architecture

The current system is a Rust-first bounded Loop Engineer research/control plane plus a separately owned deterministic Rust trading runtime.

Monday is one multi-venue trading system. Polymarket, Binance, OKX, and other
exchanges are venue Adapters at the existing market-data and execution seams;
they are not separate product authorities. `ploy-*` crate and binary names are
temporary compatibility names inside the prediction-market module. Do not add a
new `products/ploy` tree or a second order, risk, reconciliation, or execution
path there. Existing compatibility contracts are migration debt and may only
shrink; they have no concrete live venue Adapter.

Research code may emit typed candidates and signed deployment envelopes. It must not import execution adapters, submit orders, broadcast transactions, increase risk caps, or resume a paused runtime.

Read [README.md](README.md), [rust_hft/ARCHITECTURE.md](rust_hft/ARCHITECTURE.md), [rust_hft/alpha-harness/README.md](rust_hft/alpha-harness/README.md), and [docs/architecture/REPOSITORY_LAYOUT.md](docs/architecture/REPOSITORY_LAYOUT.md) before architecture changes.

## Engineering Rules

- Prefer Rust for durable production paths.
- Repository research, data, training, evaluation, and runtime code is Rust-only.
  Do not add Python or PyTorch/libtorch bindings; missing capability fails closed
  until a governed Rust implementation exists.
- Never silently replace real data with fixtures.
- Keep dataset, candidate, evaluation, approval, policy, feedback, and deployment evidence content-addressed or append-only.
- Keep private signing keys and LLM credentials out of DuckDB and logs.
- Live-small activation remains fail-closed until every order path consumes envelope order-size and slippage limits.

## Agent skills

### Issue tracker

Issues are tracked in GitHub Issues for `proerror77/monday`; external pull requests are not a triage request surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the canonical `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix` labels. See `docs/agents/triage-labels.md`.

### Issue lifecycle

GitHub issue metadata is the source of truth. Follow the publication,
relationship, ownership, and closure contract in
`docs/agents/issue-tracker.md`.

### Domain docs

Use the repository's single-context domain-doc layout. See `docs/agents/domain.md`.
