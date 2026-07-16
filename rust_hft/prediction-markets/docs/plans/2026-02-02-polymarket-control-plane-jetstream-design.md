# Polymarket Control Plane Redesign (JetStream)

Date: 2026-02-02

## Context
Current pain points:
- Trading logic regressions are frequent.
- Behavior/requirements are unclear, changes are risky.
- Architecture is tightly coupled; changes ripple across the system.

Target: A distributed control plane that manages multiple strategy runtimes, with clear service boundaries, auditable commands, and safe restart/config flows.

## Goals
- Separate control plane, risk, and execution responsibilities.
- Run each strategy in its own process/container.
- Use NATS JetStream for command/event durability and replay.
- Centralized order execution and risk enforcement.
- Config is Git baseline + DB overrides with versioning.
- State is authoritative in DB with runtime reconciliation.
- Strong observability: health, metrics, audit trails.

## Non-goals (v1)
- Multi-region active/active.
- Automated strategy discovery from external registries.
- Full backtest/optimizer pipeline.

## Architecture Overview
Services:
- **Control Plane API**: lifecycle commands, config management, status aggregation.
- **Strategy Runtime** (per strategy): consumes commands, produces intents, local safety checks.
- **Risk Service** (central): enforces global and per-strategy limits, exposure, throttles.
- **Order Executor** (central): single gateway for signing and submitting orders.
- **Config Source**: Git repo for baseline configs + DB overrides.
- **State Store**: DB for trades, orders, risk events, status, audit logs.
- **NATS JetStream**: durable command/event bus.

## NATS Subjects (v1)
- `cmd.strategy.{id}`: start/stop/restart/update risk/config
- `event.strategy.{id}`: status, heartbeat, errors, metrics
- `order.request` / `order.result`: order intent -> execution result
- `risk.check` / `risk.result`: pre-trade and ongoing risk checks
- `config.change`: config updates + version

All messages carry: `trace_id`, `strategy_id`, `version`, `idempotency_key`, `timestamp`.

## Data Flows
### Start/Stop/Restart
1. Control Plane publishes `cmd.strategy.{id}` with config version.
2. Runtime acknowledges via `event.strategy.{id}` (state change + correlation id).
3. Runtime spawns strategy loop; emits heartbeats and metrics.

### Config Change
1. Config update (Git commit or DB override) -> `config.change` event.
2. Control Plane maps config -> affected strategy -> emits restart command.
3. Runtime drains, pauses new orders, reloads config, restarts loop.

### Order Flow
1. Runtime emits `order.request` with intent + idempotency key.
2. Executor calls `risk.check` and awaits `risk.result`.
3. If pass, executor signs + submits; writes to DB; emits `order.result`.
4. Runtime consumes `order.result` and updates local state.

## Lifecycle & Failure Handling
- Strategy-level failure only restarts that runtime.
- Backoff policy with max retry; hard halt on repeated failure.
- Central services (risk/executor) expose health and circuit-breakers.

## State & Reconciliation
- DB is authoritative for orders/positions.
- Runtime keeps local cache; on start, reconciles with DB to rebuild state.
- Idempotency enforced at executor + DB layer.

## Observability
- Metrics per strategy: status, latency, orders, rejects, last trade.
- Structured logs with trace_id across services.
- Control Plane exposes aggregated status endpoint.

## Security (Dev)
- Dev stage: internal-only access; production adds mTLS/JWT.

## Testing Strategy (TDD + Characterization)
- Characterization tests lock current strategy behavior.
- TDD for new control-plane components and message contracts.
- Contract tests for NATS subjects + schemas.
- Integration tests with JetStream in test containers.

## Migration Plan (High-Level)
1. Introduce message schemas + NATS client wrappers.
2. Build Control Plane API + Strategy Runtime skeletons.
3. Implement Risk Service and Order Executor.
4. Migrate one strategy end-to-end.
5. Iterate strategy-by-strategy.
