# Project Overview
- High-frequency trading (HFT) platform implemented in Rust, targeting microsecond-level execution with modular crates for data ingestion, aggregation, risk, execution, and runtime orchestration.
- Workspace-managed monorepo with numerous crates (`crates/engine`, `crates/data`, `crates/execution`, `strategies/*`, etc.) and `apps/*` entrypoints for live, paper, and replay trading modes.
- Architecture follows clear separation between core engine, adapters, strategies, and infrastructure (monitoring, storage, deployment). Feature flags control optional integrations.
- Designed to integrate with external services (ClickHouse, Redis, Prometheus/Grafana) managed via Docker Compose under `ops/`.
- Documentation in `ARCHITECTURE.md` outlines responsibilities, naming, and development guidelines; additional guides located in `docs/` and top-level markdown files.