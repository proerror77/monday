# Suggested Commands
- `cargo build --release` — build the full workspace binaries.
- `cargo run -p hft-live --features="bitget,trend-strategy,metrics"` — launch live trading app with key features.
- `cargo test`, `cargo test --lib`, `cargo test --test '*'` — run all/unit/integration tests.
- `cargo check --all-targets --all-features` + `cargo clippy --all-targets --all-features -- -D warnings` — lint and static checks.
- `cargo fmt` or `cargo fmt -- --check` — apply or verify formatting.
- `make dev` / `make down` — start/stop full dockerized development stack (ClickHouse, Prometheus, Grafana, etc.).
- `make metrics` / `make health` — query Prometheus metrics or service health probes.
- `docker-compose` commands under `ops/` for manual service control when needed.