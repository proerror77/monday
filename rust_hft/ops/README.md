# Legacy Local Operations Assets

`rust_hft/ops/` contains local ClickHouse, monitoring, collector, and host-tuning assets retained for development and migration. It is not the production deployment entry point.

Use these canonical paths instead:

- Production images and Kubernetes: [`../deployment/PRODUCTION_DEPLOYMENT.md`](../deployment/PRODUCTION_DEPLOYMENT.md)
- Runtime architecture and safety boundaries: [`../ARCHITECTURE.md`](../ARCHITECTURE.md)
- SLO and incident response: [`../docs/slo_runbook.md`](../docs/slo_runbook.md)
- Current local compose contract: [`../../deploy/docker-compose.yml`](../../deploy/docker-compose.yml)

## Guardrails

- Never put credentials in this directory, a compose file, systemd unit, or tracked `.env` file.
- Do not use the legacy compose files as evidence that the signed deployment handoff, reconciliation, or runtime state volumes are configured.
- Do not expose Grafana, ClickHouse, Redis, metrics, or gRPC ports publicly by default.
- Do not treat a running collector or dashboard as proof of research data quality or alpha.
- Paper and Shadow are the only Agent-produced runtime activation modes. Live-small remains disabled.

## Supported Preflight

From the repository root:

```bash
cargo test --manifest-path rust_hft/Cargo.toml -p hft-infra-secrets --test tracked_secrets_contract --locked -- --nocapture
cargo test --manifest-path rust_hft/Cargo.toml -p hft-live --no-default-features --test deployment_artifacts --locked
```

From `rust_hft/`:

```bash
cargo test --locked -p hft-live --no-default-features --test deployment_envelope
cargo test --locked -p hft-live --no-default-features --features formula-strategy,bitget --test deployment_artifacts
```

Any future production use of an asset under this directory must first move it into `rust_hft/deployment/`, add a fail-closed contract check, and pass the production acceptance matrix.
