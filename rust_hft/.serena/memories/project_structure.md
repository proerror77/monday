# Project Structure
- `apps/`: executable entrypoints (`live`, `paper`, `replay`, plus adapters) configured through feature flags.
- `crates/`: core libraries—`core` (base types), `ports` (traits/events), `engine` (single-writer event loop), `data` (market data + adapters), `execution` (OMS/exchange clients), `risk`, `runtime`, `infra/*` for integrations, `testing`, `recovery`, etc.
- `strategies/`: strategy implementations (trend, arbitrage, imbalance, lob_flow_grid) depending on `ports` + `core`.
- `config/`: environment-specific configuration (dev/staging/prod) referenced by runtime.
- `ops/`: Docker Compose stacks, ClickHouse initialization, monitoring dashboards, deployment scripts.
- Top-level docs (`ARCHITECTURE.md`, `performance_analysis_report.md`, etc.) capture architecture, performance, and operational guidance.