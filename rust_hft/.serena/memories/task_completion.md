# Task Completion Checklist
- Ensure code compiles: `cargo check --all-targets --all-features`.
- Run `cargo fmt` and `cargo clippy -- -D warnings` to maintain style and lint cleanliness.
- Execute relevant `cargo test` suites (unit/integration/strategy-specific) for affected crates.
- Update documentation or configuration notes if behavior changes (`ARCHITECTURE.md`, guides under `docs/`, etc.).
- If Docker services are involved, verify local stack (`make dev`/`make status`) and metrics where applicable.