# tango-1-1 DB Schema & Code Map

> **Historical standalone PLOY operations.** Host, systemd, secret, and nested
> workflow commands below are retained as migration evidence only. They do not
> define a current Monday deployment or execution path.

- Generated at: 2026-03-02T05:23:49Z
- Host: tango-1-1
- Database: ploy (via /root/ploy/.env -> DATABASE_URL)
- Skills used:
  - wshobson/agents@postgresql-table-design
  - wshobson/agents@database-migration
  - aj-geddes/useful-ai-prompts@database-schema-documentation

## Summary

- Base tables: 64
- Views: 8
- Foreign keys: 13

## Largest Tables (Top 15)

| Table | est_rows | total_size | table_size | index_size |
|---|---:|---:|---:|---:|
| clob_trade_ticks | 265230 | 11 GB | 6888 MB | 4476 MB |
| clob_quote_ticks | 43009 | 3522 MB | 1683 MB | 1839 MB |
| binance_lob_ticks | 0 | 1938 MB | 1842 MB | 95 MB |
| backtest_signals | 0 | 1893 MB | 1567 MB | 326 MB |
| clob_orderbook_snapshots | 7849 | 1274 MB | 1156 MB | 118 MB |
| sync_records | 4595460 | 1001 MB | 637 MB | 364 MB |
| binance_price_ticks | 22380 | 312 MB | 160 MB | 152 MB |
| pm_token_settlements | 8448 | 45 MB | 2240 kB | 2840 kB |
| pm_market_metadata | 2638 | 17 MB | 576 kB | 840 kB |
| signal_history | 10 | 13 MB | 12 MB | 1792 kB |
| risk_gate_decisions | 0 | 12 MB | 11 MB | 1400 kB |
| collector_token_targets | 22164 | 12 MB | 9104 kB | 2712 kB |
| agent_order_executions | 0 | 8016 kB | 6264 kB | 1600 kB |
| execution_analysis | 0 | 7744 kB | 6656 kB | 936 kB |
| binance_klines | 0 | 6832 kB | 4632 kB | 2160 kB |

## Key Foreign Keys

| Table | Column | Ref Table | Ref Column |
|---|---|---|---|
| backtest_signals | run_id | backtest_runs | run_id |
| backtest_trades | run_id | backtest_runs | run_id |
| cycles | round_id | rounds | id |
| dump_signals | round_id | rounds | id |
| fills | order_id | orders | id |
| fills | position_id | positions | id |
| orders | cycle_id | cycles | id |
| position_discrepancies | reconciliation_id | position_reconciliation_log | id |
| recovery_attempts | checkpoint_id | checkpoints | id |
| state_transitions | cycle_id | cycles | id |
| strategy_state | current_cycle_id | cycles | id |
| strategy_state | current_round_id | rounds | id |
| ticks | round_id | rounds | id |

## Table Inventory

| Table | PK | Column Count |
|---|---|---:|
| _sqlx_migrations | version | 6 |
| accounts | account_id | 6 |
| agent_order_executions | id | 21 |
| backtest_runs | run_id | 32 |
| backtest_signals | id | 17 |
| backtest_trades | id | 21 |
| balance_snapshots | id | 5 |
| binance_klines | id | 13 |
| binance_lob_ticks | id | 16 |
| binance_price_ticks | id | 6 |
| chainlink_price_ticks | id | 5 |
| checkpoints | id | 6 |
| clob_orderbook_snapshots | id | 11 |
| clob_quote_ticks | id | 10 |
| clob_trade_alerts | id | 16 |
| clob_trade_ticks | id | 17 |
| collector_token_targets | token_id | 7 |
| component_heartbeats | component_name | 8 |
| component_restarts | id | 7 |
| coordinator_governance_policies | account_id | 8 |
| coordinator_governance_policy_history | id | 9 |
| cycles | id | 15 |
| daily_metrics | date | 12 |
| dead_letter_queue | id | 12 |
| dump_signals | id | 11 |
| event_projections | id | 6 |
| event_registry | id | 19 |
| event_snapshots | id | 7 |
| execution_analysis | id | 20 |
| exit_reasons | id | 18 |
| fills | id | 9 |
| grok_game_intel | id | 22 |
| market_window_labels | id | 8 |
| nba_comeback_agent_state | account_id,agent_id | 4 |
| nba_comeback_trades | id | 22 |
| nba_live_observations | id | 37 |
| nba_schedule_calendar | espn_game_id | 11 |
| nba_team_stats | id | 22 |
| nonce_counter | wallet_address | 4 |
| nonce_state | id | 3 |
| nonce_usage | id | 9 |
| order_idempotency | id | 11 |
| orders | id | 21 |
| pm_market_metadata | market_slug | 8 |
| pm_token_settlements | token_id | 10 |
| position_discrepancies | id | 10 |
| position_reconciliation_log | id | 6 |
| positions | id | 15 |
| quote_freshness | id | 9 |
| recovery_attempts | id | 9 |
| risk_gate_decisions | id | 13 |
| risk_runtime_state | account_id | 10 |
| rounds | id | 9 |
| security_audit_log | id | 9 |
| signal_history | id | 22 |
| state_snapshots | id | 7 |
| state_transitions | id | 9 |
| strategy_evaluations | id | 21 |
| strategy_events | id | 8 |
| strategy_state | id | 6 |
| sync_records | id | 18 |
| system_events | id | 7 |
| ticks | id | 8 |
| watchdog_alerts | id | 10 |

## Related Code by Table

### _sqlx_migrations

Referenced files:

- `src/coordinator/bootstrap.rs`

### accounts

Referenced files:

- `migrations/014_multi_account_and_collector_targets.sql`
- `src/api/handlers/system.rs`
- `src/api/routes.rs`
- `src/api/types.rs`
- `src/config.rs`
- `src/coordinator/bootstrap.rs`
- `src/platform/contracts.rs`
- `src/strategy/calculations.rs`
- `src/strategy/volatility.rs`

### agent_order_executions

Referenced files:

- `migrations/014_multi_account_and_collector_targets.sql`
- `scripts/ploy_maintenance.sh`
- `scripts/report_drawdown.py`
- `scripts/train_crypto_lob_tcn_onnx_from_db.py`
- `src/cli/strategy.rs`
- `src/coordinator/bootstrap.rs`
- `src/coordinator/coordinator.rs`

### backtest_runs

Referenced files:

- `migrations/019_data_integrity.sql`
- `migrations/021_backtest_tables.sql`
- `src/cli/strategy.rs`
- `src/strategy/backtest_recorder.rs`
- `src/strategy/backtest_report.rs`
- `src/strategy/momentum_backtest.rs`

### backtest_signals

Referenced files:

- `migrations/021_backtest_tables.sql`
- `src/strategy/backtest_recorder.rs`
- `src/strategy/backtest_report.rs`

### backtest_trades

Referenced files:

- `migrations/021_backtest_tables.sql`
- `src/cli/strategy.rs`
- `src/strategy/backtest_recorder.rs`
- `src/strategy/backtest_report.rs`

### balance_snapshots

Referenced files:

- `migrations/006_position_management.sql`
- `migrations/007_performance_indexes.sql`
- `migrations/013_schema_repair_and_observability.sql`
- `src/coordinator/bootstrap.rs`

### binance_klines

Referenced files:

- `migrations/018_training_data_tables.sql`
- `src/cli/strategy.rs`
- `src/collector/backtest_collector.rs`
- `src/collector/binance_klines.rs`
- `src/collector/mod.rs`
- `src/coordinator/bootstrap.rs`
- `src/platform/subscription_planner.rs`
- `src/strategy/backtest_feed.rs`
- `src/strategy/feeds.rs`

### binance_lob_ticks

Referenced files:

- `migrations/018_training_data_tables.sql`
- `scripts/ploy_maintenance.sh`
- `src/coordinator/bootstrap.rs`
- `src/platform/persistence_pipeline.rs`

### binance_price_ticks

Referenced files:

- `migrations/018_training_data_tables.sql`
- `scripts/ploy_maintenance.sh`
- `src/agents/crypto_lob_ml.rs`
- `src/coordinator/bootstrap.rs`
- `src/platform/persistence_pipeline.rs`
- `src/strategy/backtest_feed.rs`

### chainlink_price_ticks

Referenced files:

- `migrations/020_chainlink_tables.sql`
- `src/platform/persistence_pipeline.rs`

### checkpoints

Referenced files:

- `migrations/003_supervisor_tables.sql`
- `src/persistence/checkpoint.rs`
- `src/rl/config.rs`
- `src/rl/training/checkpointing.rs`

### clob_orderbook_snapshots

Referenced files:

- `migrations/018_training_data_tables.sql`
- `scripts/ploy_maintenance.sh`
- `src/agents/crypto_lob_ml.rs`
- `src/agents/sports.rs`
- `src/analysis/updown_backtest.rs`
- `src/cli/runtime.rs`
- `src/coordinator/bootstrap.rs`
- `src/platform/persistence_pipeline.rs`
- `src/strategy/backtest_feed.rs`

### clob_quote_ticks

Referenced files:

- `deployment/bin/ploy-orderbook-history-collector.sh`
- `deployment/env.crypto-dryrun.example`
- `migrations/010_clob_quote_ticks.sql`
- `scripts/ploy_maintenance.sh`
- `src/api/state.rs`
- `src/cli/strategy.rs`
- `src/coordinator/bootstrap.rs`
- `src/platform/persistence_pipeline.rs`
- `src/strategy/backtest_feed.rs`

### clob_trade_alerts

Referenced files:

- `scripts/ploy_maintenance.sh`
- `scripts/pm_trade_alerts_watch.sh`
- `src/coordinator/bootstrap.rs`

### clob_trade_ticks

Referenced files:

- `migrations/018_training_data_tables.sql`
- `scripts/ploy_maintenance.sh`
- `scripts/pm_bursts_watch.sh`
- `scripts/pm_trades_watch.sh`
- `src/agents/crypto_lob_ml.rs`
- `src/coordinator/bootstrap.rs`

### collector_token_targets

Referenced files:

- `deployment/bin/ploy-orderbook-history-collector.sh`
- `deployment/env.crypto-dryrun.example`
- `migrations/014_multi_account_and_collector_targets.sql`
- `src/agents/sports.rs`
- `src/collector/polymarket_orderbook_history.rs`
- `src/collector/token_targets.rs`
- `src/coordinator/bootstrap.rs`
- `src/main_modes/collector_modes.rs`

### component_heartbeats

Referenced files:

- `migrations/002_reliability_foundation.sql`
- `migrations/003_supervisor_tables.sql`
- `migrations/007_performance_indexes.sql`
- `migrations/013_schema_repair_and_observability.sql`
- `src/adapters/transaction_manager.rs`
- `src/coordinator/bootstrap.rs`

### component_restarts

Referenced files:

- `migrations/003_supervisor_tables.sql`

### coordinator_governance_policies

Referenced files:

- `src/coordinator/bootstrap.rs`
- `src/coordinator/coordinator.rs`

### coordinator_governance_policy_history

Referenced files:

- `src/coordinator/bootstrap.rs`
- `src/coordinator/coordinator.rs`

### cycles

Referenced files:

- `migrations/001_init.sql`
- `migrations/005_idempotency_and_security.sql`
- `migrations/016_security_fixes_legacy.sql`
- `src/adapters/postgres.rs`
- `src/adapters/transaction_manager.rs`
- `src/ai_clients/autonomous.rs`
- `src/ai_clients/protocol.rs`
- `src/api/handlers/stats.rs`
- `src/api/handlers/strategies.rs`
- `src/api/state.rs`
- `src/persistence/dlq_processor.rs`
- `src/api/handlers/system.rs`
- `src/strategy/event_edge/core.rs`
- `src/strategy/execution/engine.rs`
- `src/strategy/execution/engine_store.rs`
- `src/strategy/feeds.rs`
- `src/strategy/nba_comeback/core.rs`

### daily_metrics

Referenced files:

- `migrations/001_init.sql`
- `src/adapters/postgres.rs`
- `src/api/handlers/sidecar.rs`
- `src/api/handlers/strategies.rs`

### dead_letter_queue

Referenced files:

- `migrations/002_reliability_foundation.sql`
- `migrations/007_performance_indexes.sql`
- `migrations/019_data_integrity.sql`
- `src/adapters/transaction_manager.rs`

### dump_signals

Referenced files:

- `migrations/001_init.sql`
- `src/adapters/postgres.rs`

### event_projections

Referenced files:

- `migrations/004_event_sourcing.sql`

### event_registry

Referenced files:

- `migrations/009_event_registry.sql`
- `src/adapters/postgres.rs`
- `src/strategy/registry/mod.rs`

### event_snapshots

Referenced files:

- `migrations/004_event_sourcing.sql`

### execution_analysis

Referenced files:

- `migrations/013_schema_repair_and_observability.sql`
- `migrations/014_multi_account_and_collector_targets.sql`
- `src/coordinator/bootstrap.rs`
- `src/coordinator/coordinator.rs`

### exit_reasons

Referenced files:

- `migrations/013_schema_repair_and_observability.sql`
- `migrations/014_multi_account_and_collector_targets.sql`
- `scripts/report_drawdown.py`
- `src/coordinator/bootstrap.rs`
- `src/coordinator/coordinator.rs`

### fills

Referenced files:

- `migrations/006_position_management.sql`
- `migrations/007_performance_indexes.sql`
- `migrations/013_schema_repair_and_observability.sql`
- `migrations/018_training_data_tables.sql`
- `migrations/019_data_integrity.sql`
- `src/adapters/kalshi_rest.rs`
- `src/adapters/polymarket_clob.rs`
- `src/config.rs`
- `src/coordinator/bootstrap.rs`
- `src/coordinator/coordinator.rs`
- `src/main_commands/crypto.rs`
- `src/main_modes/collector_modes.rs`
- `src/platform/agents/nba_agent.rs`
- `src/rl/integration/rl_strategy.rs`
- `src/strategy/adapters.rs`
- `src/strategy/backtest_feed.rs`
- `src/strategy/dump_hedge.rs`
- `src/strategy/execution/engine.rs`
- `src/strategy/execution/executor.rs`
- ... and 6 more files

### grok_game_intel

Referenced files:

- `migrations/014_multi_account_and_collector_targets.sql`
- `src/agents/sports.rs`

### market_window_labels

Referenced files:

- `migrations/020_chainlink_tables.sql`

### nba_comeback_agent_state

Referenced files:

- `src/agents/sports.rs`

### nba_comeback_trades

Referenced files:

- `migrations/008_nba_comeback.sql`

### nba_live_observations

Referenced files:

- `migrations/011_nba_live_observations.sql`
- `migrations/014_multi_account_and_collector_targets.sql`
- `scripts/ploy_maintenance.sh`
- `src/agents/sports.rs`

### nba_schedule_calendar

Referenced files:

- `migrations/012_nba_schedule_calendar.sql`
- `src/agents/sports.rs`

### nba_team_stats

Referenced files:

- `deployment/env.sports-live.example`
- `deployment/env.sports-pm.example`
- `migrations/008_nba_comeback.sql`
- `src/adapters/postgres.rs`
- `src/cli/strategy.rs`
- `src/strategy/nba_comeback/comeback_stats.rs`

### nonce_counter

Referenced files:

- `migrations/016_security_fixes_legacy.sql`

### nonce_state

Referenced files:

- `migrations/005_idempotency_and_security.sql`

### nonce_usage

Referenced files:

- `migrations/007_performance_indexes.sql`
- `migrations/013_schema_repair_and_observability.sql`
- `migrations/016_security_fixes_legacy.sql`
- `src/coordinator/bootstrap.rs`
- `src/signing/nonce_manager.rs`

### order_idempotency

Referenced files:

- `migrations/005_idempotency_and_security.sql`
- `migrations/007_performance_indexes.sql`
- `migrations/013_schema_repair_and_observability.sql`
- `migrations/014_multi_account_and_collector_targets.sql`
- `migrations/016_security_fixes_legacy.sql`
- `src/coordinator/bootstrap.rs`
- `src/strategy/execution/idempotency.rs`

### orders

Referenced files:

- `deployment/env.example`
- `deployment/env.sports-pm.example`
- `migrations/001_init.sql`
- `migrations/006_position_management.sql`
- `migrations/007_performance_indexes.sql`
- `migrations/013_schema_repair_and_observability.sql`
- `migrations/016_security_fixes_legacy.sql`
- `migrations/019_data_integrity.sql`
- `migrations/022_order_strategy_tracking.sql`
- `scripts/dry-run-platform-smoke.sh`
- `src/adapters/kalshi_rest.rs`
- `src/adapters/polymarket_clob.rs`
- `src/adapters/polymarket_official.rs`
- `src/adapters/postgres.rs`
- `src/adapters/transaction_manager.rs`
- `src/agents/context.rs`
- `src/agents/crypto.rs`
- `src/agents/crypto_lob_ml.rs`
- ... and 68 more files

### pm_market_metadata

Referenced files:

- `migrations/018_training_data_tables.sql`
- `scripts/train_crypto_lob_tcn_onnx_from_db.py`
- `src/cli/strategy.rs`
- `src/coordinator/bootstrap.rs`
- `src/strategy/backtest_feed.rs`
- `src/strategy/feeds.rs`

### pm_token_settlements

Referenced files:

- `deployment/env.crypto-collector.example`
- `deployment/env.crypto-dryrun.example`
- `migrations/015_pm_token_settlements.sql`
- `migrations/018_training_data_tables.sql`
- `scripts/train_crypto_lob_tcn_onnx_from_db.py`
- `src/cli/strategy.rs`
- `src/coordinator/bootstrap.rs`
- `src/strategy/backtest_feed.rs`

### position_discrepancies

Referenced files:

- `migrations/006_position_management.sql`
- `migrations/007_performance_indexes.sql`
- `migrations/013_schema_repair_and_observability.sql`
- `migrations/019_data_integrity.sql`
- `src/coordinator/bootstrap.rs`

### position_reconciliation_log

Referenced files:

- `migrations/006_position_management.sql`
- `migrations/007_performance_indexes.sql`
- `migrations/013_schema_repair_and_observability.sql`
- `src/coordinator/bootstrap.rs`

### positions

Referenced files:

- `migrations/002_reliability_foundation.sql`
- `migrations/006_position_management.sql`
- `migrations/007_performance_indexes.sql`
- `migrations/013_schema_repair_and_observability.sql`
- `migrations/019_data_integrity.sql`
- `migrations/021_backtest_tables.sql`
- `src/adapters/kalshi_rest.rs`
- `src/adapters/onchain_indexer.rs`
- `src/adapters/polymarket_clob.rs`
- `src/agents/crypto.rs`
- `src/agents/crypto_lob_ml.rs`
- `src/agents/crypto_rl_policy.rs`
- `src/agents/openclaw/agent.rs`
- `src/agents/openclaw/conflict.rs`
- `src/agents/openclaw/straddle.rs`
- `src/agents/politics.rs`
- `src/agents/sports.rs`
- `src/ai_clients/autonomous.rs`
- ... and 71 more files

### quote_freshness

Referenced files:

- `migrations/005_idempotency_and_security.sql`
- `src/coordinator/bootstrap.rs`

### recovery_attempts

Referenced files:

- `migrations/003_supervisor_tables.sql`

### risk_gate_decisions

Referenced files:

- `migrations/013_schema_repair_and_observability.sql`
- `migrations/014_multi_account_and_collector_targets.sql`
- `src/coordinator/bootstrap.rs`
- `src/coordinator/coordinator.rs`

### risk_runtime_state

Referenced files:

- `src/api/handlers/sidecar.rs`
- `src/coordinator/bootstrap.rs`
- `src/coordinator/coordinator.rs`

### rounds

Referenced files:

- `migrations/001_init.sql`
- `src/adapters/postgres.rs`
- `src/signing/order.rs`
- `src/strategy/execution/engine.rs`

### security_audit_log

Referenced files:

- `migrations/005_idempotency_and_security.sql`
- `src/api/handlers/sidecar.rs`
- `src/api/handlers/system.rs`

### signal_history

Referenced files:

- `migrations/013_schema_repair_and_observability.sql`
- `migrations/014_multi_account_and_collector_targets.sql`
- `scripts/report_drawdown.py`
- `src/cli/strategy.rs`
- `src/coordinator/bootstrap.rs`
- `src/coordinator/coordinator.rs`
- `src/strategy/adapters.rs`

### state_snapshots

Referenced files:

- `migrations/002_reliability_foundation.sql`
- `src/adapters/transaction_manager.rs`

### state_transitions

Referenced files:

- `migrations/007_performance_indexes.sql`
- `migrations/016_security_fixes_legacy.sql`
- `src/strategy/strategies/two_leg.rs`

### strategy_evaluations

Referenced files:

- `migrations/017_strategy_evaluations.sql`
- `migrations/019_data_integrity.sql`
- `src/api/handlers/deployments.rs`
- `src/api/handlers/evaluations.rs`
- `src/api/handlers/mod.rs`
- `src/api/handlers/strategies.rs`
- `src/api/handlers/strategy_evaluations.rs`
- `src/api/routes.rs`
- `src/api/state.rs`
- `src/coordinator/bootstrap.rs`
- `src/coordinator/coordinator.rs`
- `src/strategy/momentum_backtest.rs`

### strategy_events

Referenced files:

- `migrations/004_event_sourcing.sql`
- `migrations/019_data_integrity.sql`
- `src/persistence/event_store.rs`

### strategy_state

Referenced files:

- `migrations/001_init.sql`
- `src/adapters/postgres.rs`
- `src/ai_clients/advisor.rs`
- `src/ai_clients/protocol.rs`
- `src/api/handlers/system.rs`
- `src/strategy/execution/engine.rs`
- `src/strategy/execution/engine_store.rs`
- `src/tui/app.rs`
- `src/tui/data.rs`
- `src/tui/mod.rs`
- `src/tui/runner.rs`
- `src/tui/widgets/footer.rs`

### sync_records

Referenced files:

- `migrations/018_training_data_tables.sql`
- `scripts/train_crypto_lob_tcn_onnx_from_db.py`
- `src/collector/sync_collector.rs`
- `src/coordinator/bootstrap.rs`
- `src/main_commands/rl/lead_lag.rs`
- `src/strategy/backtest_feed.rs`

### system_events

Referenced files:

- `migrations/002_reliability_foundation.sql`
- `migrations/003_supervisor_tables.sql`
- `migrations/005_idempotency_and_security.sql`
- `migrations/007_performance_indexes.sql`
- `migrations/013_schema_repair_and_observability.sql`
- `src/adapters/transaction_manager.rs`
- `src/coordination/emergency_stop.rs`
- `src/coordinator/bootstrap.rs`

### ticks

Referenced files:

- `deployment/bin/ploy-orderbook-history-collector.sh`
- `deployment/env.crypto-dryrun.example`
- `deployment/ploy-crypto-collector.service`
- `migrations/001_init.sql`
- `migrations/010_clob_quote_ticks.sql`
- `migrations/018_training_data_tables.sql`
- `migrations/019_data_integrity.sql`
- `migrations/020_chainlink_tables.sql`
- `scripts/ploy_maintenance.sh`
- `scripts/pm_bursts_watch.sh`
- `scripts/pm_trades_watch.sh`
- `src/adapters/postgres.rs`
- `src/agents/crypto_lob_ml.rs`
- `src/agents/openclaw/config.rs`
- `src/agents/openclaw/regime.rs`
- `src/api/state.rs`
- `src/cli/strategy.rs`
- ... and 18 more files

### watchdog_alerts

Referenced files:

- `migrations/003_supervisor_tables.sql`

## Tables Without Direct References in src/migrations/scripts/deployment


## Views

- `v_active_issues`
- `v_event_statistics`
- `v_failed_transitions`
- `v_idempotency_stats`
- `v_nonce_stats`
- `v_position_stats`
- `v_recent_reconciliations`
- `v_unresolved_discrepancies`
