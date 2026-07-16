# tango-1-1 Mermaid ERD

> **Historical standalone PLOY operations.** Host, systemd, secret, and nested
> workflow commands below are retained as migration evidence only. They do not
> define a current Monday deployment or execution path.

- Generated at: 2026-03-02T05:23:07Z
- Source DB: `tango-1-1` / `ploy`
- Note: Diagram A uses enforced FK only. Diagram B adds logical (application-level) links.

## Diagram A: Enforced FK ERD (Ground Truth)

```mermaid
erDiagram
    rounds ||--o{ cycles : "round_id"
    rounds ||--o{ ticks : "round_id"
    rounds ||--o{ dump_signals : "round_id"

    cycles ||--o{ orders : "cycle_id"
    cycles ||--o{ state_transitions : "cycle_id"

    orders ||--o{ fills : "order_id"
    positions ||--o{ fills : "position_id"

    rounds ||--o{ strategy_state : "current_round_id"
    cycles ||--o{ strategy_state : "current_cycle_id"

    position_reconciliation_log ||--o{ position_discrepancies : "reconciliation_id"
    checkpoints ||--o{ recovery_attempts : "checkpoint_id"

    backtest_runs ||--o{ backtest_signals : "run_id"
    backtest_runs ||--o{ backtest_trades : "run_id"

    rounds {
        int id PK
        text slug
        timestamptz start_time
        timestamptz end_time
    }

    cycles {
        int id PK
        int round_id FK
        text state
        numeric pnl
    }

    orders {
        int id PK
        int cycle_id FK
        text token_id
        text status
        numeric limit_price
        int filled_shares
    }

    positions {
        int id PK
        text token_id
        text status
        bigint shares
        numeric avg_entry_price
    }

    fills {
        int id PK
        int order_id FK
        int position_id FK
        numeric price
        int shares
        timestamptz timestamp
    }

    ticks {
        bigint id PK
        int round_id FK
        text side
        timestamptz timestamp
    }

    dump_signals {
        int id PK
        int round_id FK
        text side
        numeric trigger_price
    }

    state_transitions {
        bigint id PK
        int cycle_id FK
        text from_state
        text to_state
        boolean success
    }

    strategy_state {
        int id PK
        int current_round_id FK
        int current_cycle_id FK
        text current_state
    }

    position_reconciliation_log {
        int id PK
        timestamptz timestamp
        int discrepancies_found
    }

    position_discrepancies {
        int id PK
        int reconciliation_id FK
        text token_id
        bigint difference
        text severity
    }

    checkpoints {
        bigint id PK
        text checkpoint_type
        text component
        jsonb data
    }

    recovery_attempts {
        int id PK
        bigint checkpoint_id FK
        text component
        text status
    }

    backtest_runs {
        uuid run_id PK
        text strategy
        text mode
        timestamptz created_at
    }

    backtest_signals {
        bigint id PK
        uuid run_id FK
        text symbol
        text signal_type
        timestamptz timestamp
    }

    backtest_trades {
        bigint id PK
        uuid run_id FK
        text symbol
        numeric pnl
        timestamptz entry_time
        timestamptz exit_time
    }
```

## Diagram B: Market Data + Research (Logical Links)

```mermaid
erDiagram
    pm_market_metadata ||--o{ pm_token_settlements : "market_slug"
    pm_token_settlements ||--o{ clob_trade_ticks : "token_id (logical)"
    pm_token_settlements ||--o{ clob_quote_ticks : "token_id (logical)"
    pm_token_settlements ||--o{ clob_orderbook_snapshots : "token_id (logical)"
    collector_token_targets ||--o{ clob_trade_ticks : "token_id/domain (logical)"

    clob_quote_ticks ||--o{ quote_freshness : "token_id (logical)"
    clob_trade_ticks ||--o{ signal_history : "token_id/market_slug (logical)"
    signal_history ||--o{ strategy_evaluations : "strategy/domain/time (logical)"

    binance_price_ticks ||--o{ sync_records : "symbol+time (logical)"
    binance_lob_ticks ||--o{ sync_records : "symbol+time (logical)"
    chainlink_price_ticks ||--o{ sync_records : "symbol+time (logical)"

    pm_market_metadata {
        text market_slug PK
        numeric price_to_beat
        timestamptz start_time
        timestamptz end_time
        text symbol
    }

    pm_token_settlements {
        text token_id PK
        text market_slug
        text condition_id
        numeric settled_price
        boolean resolved
    }

    clob_trade_ticks {
        bigint id PK
        text token_id
        text condition_id
        numeric price
        numeric size
        timestamptz trade_ts
    }

    clob_quote_ticks {
        bigint id PK
        text token_id
        text side
        numeric best_bid
        numeric best_ask
        timestamptz received_at
    }

    clob_orderbook_snapshots {
        bigint id PK
        text token_id
        jsonb bids
        jsonb asks
        timestamptz received_at
    }

    collector_token_targets {
        text token_id PK
        text domain
        date target_date
        timestamptz expires_at
    }

    quote_freshness {
        int id PK
        text token_id
        text side
        boolean is_stale
        timestamptz received_at
    }

    signal_history {
        bigint id PK
        text strategy_id
        text domain
        text market_slug
        text token_id
        numeric edge
        timestamptz recorded_at
    }

    strategy_evaluations {
        bigint id PK
        text strategy_id
        text domain
        text status
        numeric score
        timestamptz evaluated_at
    }

    binance_price_ticks {
        bigint id PK
        text symbol
        numeric price
        timestamptz trade_time
    }

    binance_lob_ticks {
        bigint id PK
        text symbol
        numeric best_bid
        numeric best_ask
        timestamptz event_time
    }

    chainlink_price_ticks {
        bigint id PK
        text symbol
        numeric price
        timestamptz source_timestamp
    }

    sync_records {
        bigint id PK
        varchar symbol
        numeric bn_mid_price
        numeric pm_yes_price
        timestamptz timestamp
    }
```

## Quick Read

- Trading execution chain (strict FK): `rounds -> cycles -> orders -> fills -> positions`
- Backtest chain (strict FK): `backtest_runs -> backtest_signals/backtest_trades`
- Most large-volume tables are market data tables (`clob_*`, `binance_*`, `sync_records`) and are linked mostly by logical keys (`token_id`, `symbol`, `market_slug`) rather than FK constraints.
