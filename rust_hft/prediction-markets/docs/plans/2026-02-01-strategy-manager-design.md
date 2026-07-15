# Strategy Manager Design (Ploy Polymarket Trading System)

Date: 2026-02-01

## Context
Current pain points:
- Trading logic regressions are frequent.
- Behavior/requirements are unclear, changes are risky.
- Architecture is tightly coupled; changes ripple across the system.

Target: A Strategy Manager that controls and monitors multiple strategies in-process, while exposing API + CLI controls, with configuration as a file-based single source of truth.

## Goals
- Single control plane for all strategies (API/CLI -> StrategyManager).
- Run multiple strategies concurrently with per-strategy risk controls.
- Config changes trigger automatic strategy restart.
- Preserve existing positions on config restart (no forced close).
- Isolate failures to a single strategy, with auto-restart + backoff.
- Unified health and metrics from a single source of truth.
- Make behavior explicit and testable to reduce regressions.

## Non-goals (for v1)
- Splitting control plane into a separate service.
- Complex state migration of live positions between strategy versions.
- Full-featured backtest/optimizer pipeline.

## Architecture Overview
In-process StrategyManager with API + CLI:

```
API/CLI -> CommandBus -> StrategyManager -> StrategyRuntime (per strategy)
                                 |-> StrategyRegistry
                                 |-> StatusBus (health/metrics)

FeedAdapter -> MarketEvent -> EventBus -> StrategyRuntime
StrategyRuntime -> RiskGuard -> OrderExecutor -> Store
```

### Core Components
1) StrategyManager
- Owns StrategyRegistry and active StrategyRuntimes.
- Receives Start/Stop/Restart/UpdateConfig commands.
- Enforces lifecycle state machine per strategy.
- Aggregates health, metrics, and status events.

2) StrategyRegistry
- Stores strategy metadata, config version, file path.
- Maps strategy id -> runtime factory.

3) StrategyRuntime
- Encapsulates a single strategy instance.
- Owns feed subscriptions, signal/decision loop, risk guard, and executor.
- Emits status/metrics events to StatusBus.

4) CommandBus
- API/CLI entrypoint to issue commands.

5) ConfigService
- Watches config files.
- On change, triggers Restart for the affected strategy.

6) EventBus
- Standardizes MarketEvent for all strategies.
- Prevents strategies from coupling to external adapters.

7) RiskGuard (per strategy)
- Per-strategy exposure limits, throttles, and circuit breakers.

8) Store
- Persists trades, errors, audit logs, and risk events.

## Data Flow
1) FeedAdapter normalizes external data -> MarketEvent.
2) EventBus distributes MarketEvent to subscribed strategies.
3) StrategyRuntime computes Intent (buy/sell/hold).
4) RiskGuard verifies intent (per-strategy limits).
5) OrderExecutor submits orders; emits outcome events.
6) Store persists trade results and audit events.
7) StatusBus publishes metrics/health for API/Prometheus.

## Lifecycle & Config Behavior
- Source of truth: config files.
- Config change triggers automatic Restart of the strategy.
- Restart process:
  1. Stop runtime loop.
  2. Pause new orders; existing positions remain untouched.
  3. Reload config.
  4. Start runtime with new config.

## Error Handling
- Strategy-level failures: only that strategy is stopped and auto-restarted.
- Auto-restart uses exponential backoff; if threshold exceeded, enter halt state.
- Global failure (DB outage, order executor down): halt all strategies and alert.

## Observability
- Single source of truth for health/metrics.
- API and Prometheus both read from StrategyManager status aggregator.
- Metrics per strategy:
  - status (running/stopped/error)
  - last trade time
  - order success/failure count
  - risk guard rejects
  - feed connectivity

## Testing Strategy (TDD + Characterization)
Given regression risk and unclear behavior, use dual-track testing:

1) Characterization tests (lock current behavior)
- Capture key trading decisions for momentum, split_arb, NBA Swing.
- Focus on: entry signals, exit signals, risk guard rejects, and order placement.
- These tests describe “current behavior” and guard against regressions.

2) TDD for new architecture code
- Write failing tests before StrategyManager and ConfigService implementation.
- Core tests:
  - start/stop/restart per strategy
  - config change triggers restart
  - multi-strategy isolation (one failure does not stop others)
  - per-strategy risk guard enforcement
  - status aggregator reports consistent values

## Migration Plan (High-Level)
1) Add StrategyManager scaffolding + interfaces (TDD).
2) Wrap existing strategies in StrategyRuntime adapters.
3) Introduce EventBus + MarketEvent normalization.
4) Move API/CLI to CommandBus.
5) Add ConfigService watcher and restart flow.
6) Replace duplicated health sources with StrategyManager aggregator.

## Open Questions
- Should StrategyManager own position reconciliation logic, or remain external?
- What is the max restart retry before hard halt?

## Recommendation on TDD
Yes, use TDD. Your three core pain points (regressions, unclear behavior, tight coupling) are exactly what TDD + characterization tests address.
