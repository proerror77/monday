# Monday Migration Tracker

## Goal

Integrate the maintained PLOY product workspace into Monday without transferring execution authority or enabling live trading.

## Ownership

- Main migration session: source import, provenance, authority docs, CI, validation, and archive handoff.
- Review agents: read-only standards and spec review of the completed Monday diff.

## Tasks

- [x] Import PLOY `main` at `8ce4e0f150173a44030294101f4b1371cbdf80bc`.
- [x] Preserve the seven local-only readiness documents as stale historical material.
- [x] Replace standalone agent/session state with Monday-local instructions.
- [x] Add and validate dedicated PLOY CI at the Monday repository root.
- [x] Pass repository secret scanning and focused Rust/TypeScript validation.
- [x] Complete standards/spec review and address findings.
- [ ] Merge the Monday migration PR.
- [ ] Redirect and archive the former PLOY repository.

## Polymarket public data on Monday ECS

- [x] Add a no-op strategy that consumes market data without emitting orders.
- [x] Add a fail-closed ECS config and hardened systemd service.
- [x] Build and install the Linux runner artifact.
- [x] Enable the BTC/ETH/SOL/XRP 5-minute/15-minute service and verify event rollover, token-mapped quotes, and zero intents/fills.
- [x] Archive hourly Polymarket tapes to OSS with manifest and readback verification.
- [x] Retain full visible CLOB depth in one-second snapshots for all seven configured crypto assets.
- [x] Add a collection-only metadata/trade/settlement tape and verify a complete OSS artifact.
- [x] Port the long-lived Polymarket reference collector and raw-tape uploader to Rust, and retire their Python runtime entry points.

## Safety

- Live trading stays disabled.
- Nested legacy workflows are inactive and are not deployment authority.
- No database, wallet, venue, cloud host, or trading service is mutated by this migration.
