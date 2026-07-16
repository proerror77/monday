# Binary Options Backtest System Design

**Date**: 2026-02-28
**Strategy**: directional (probability-driven binary option trading)
**Goal**: Strategy validation with DB-persisted signals, dual settlement verification, and data-driven optimization suggestions

---

## Problem

The current `DirectionalBacktestEngine` runs purely in-memory. Signals and trades are lost after each run. Settlement mode (`signal_history` table) has no `directional_entry` records because the directional strategy never writes to DB. The two paths (replay vs settlement) are disconnected.

## Design Decisions

| Decision | Choice |
|----------|--------|
| Core goal | Strategy validation (PnL, win rate, Sharpe, calibration) |
| Settlement | Dual verification: tick replay (fast, offline) + Gamma API (ground truth) |
| Signal granularity | Entry + Exit + Filtered signals (see missed opportunities) |
| Run management | `backtest_runs` table with run_id, all signals/trades FK to run_id |

---

## 1. Database Schema

Three new tables:

```sql
-- 1. Backtest run metadata
CREATE TABLE backtest_runs (
    run_id        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    strategy      TEXT NOT NULL,
    mode          TEXT NOT NULL,
    config_json   JSONB NOT NULL,
    symbols       TEXT[] NOT NULL,
    data_start    TIMESTAMPTZ,
    data_end      TIMESTAMPTZ,
    total_trades  INT,
    win_rate      DOUBLE PRECISION,
    total_pnl     NUMERIC,
    sharpe_ratio  DOUBLE PRECISION,
    max_drawdown  NUMERIC,
    profit_factor DOUBLE PRECISION,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 2. Backtest signals (entry + exit + filtered)
CREATE TABLE backtest_signals (
    id            BIGSERIAL PRIMARY KEY,
    run_id        UUID NOT NULL REFERENCES backtest_runs(run_id),
    signal_type   TEXT NOT NULL,          -- 'entry', 'exit', 'filtered'
    symbol        TEXT NOT NULL,
    direction     TEXT NOT NULL,          -- 'UP', 'DOWN'
    timestamp     TIMESTAMPTZ NOT NULL,
    p_hat         DOUBLE PRECISION,
    ev_net        DOUBLE PRECISION,
    sigma         DOUBLE PRECISION,
    market_price  NUMERIC,
    spot_price    NUMERIC,
    s0            NUMERIC,
    time_remaining_secs DOUBLE PRECISION,
    filter_reason TEXT,                   -- only for 'filtered': cooldown, max_positions, price_bounds, etc.
    exit_reason   TEXT,                   -- only for 'exit': settlement, time_stop, hard_stop, prob_stop
    exit_price    NUMERIC,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_bt_signals_run ON backtest_signals(run_id);

-- 3. Backtest closed trades
CREATE TABLE backtest_trades (
    id            BIGSERIAL PRIMARY KEY,
    run_id        UUID NOT NULL REFERENCES backtest_runs(run_id),
    symbol        TEXT NOT NULL,
    direction     TEXT NOT NULL,
    entry_time    TIMESTAMPTZ NOT NULL,
    exit_time     TIMESTAMPTZ NOT NULL,
    entry_price   NUMERIC NOT NULL,
    exit_price    NUMERIC NOT NULL,
    shares        INT NOT NULL,
    pnl           NUMERIC NOT NULL,
    won           BOOLEAN NOT NULL,
    holding_secs  BIGINT NOT NULL,
    exit_reason   TEXT NOT NULL,
    entry_p_hat   DOUBLE PRECISION,
    entry_ev_net  DOUBLE PRECISION,
    entry_sigma   DOUBLE PRECISION,
    s0            NUMERIC,
    -- Gamma settlement dual verification
    gamma_settled_price NUMERIC,
    gamma_resolved      BOOLEAN,
    gamma_match         BOOLEAN,          -- tick outcome vs Gamma agreement
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_bt_trades_run ON backtest_trades(run_id);
```

---

## 2. Engine Refactor — BacktestRecorder Trait

Decouple signal persistence from the engine via a trait:

```rust
pub trait BacktestRecorder: Send {
    fn record_entry(&mut self, signal: &BacktestSignal);
    fn record_exit(&mut self, signal: &BacktestSignal);
    fn record_filtered(&mut self, signal: &BacktestSignal, reason: &str);
    fn flush(&mut self) -> Result<()>;
}
```

Two implementations:

- **`PgBacktestRecorder`** — Batched INSERT (500 rows per batch) to `backtest_signals` and `backtest_trades`. Creates `backtest_runs` row at start, updates summary metrics at end.
- **`NullRecorder`** — No-op, for unit tests and quick iteration without DB.

Engine signature change:

```rust
impl DirectionalBacktestEngine {
    pub fn new(config: DirectionalBacktestConfig, recorder: Box<dyn BacktestRecorder>) -> Self;
    pub fn run<F: MarketFeed>(&mut self, feed: &mut F) -> BacktestResults;
}
```

Insertion points in existing engine code:

| Location | Signal Type | Data |
|----------|-------------|------|
| `try_directional_entry()` after position push | `entry` | p_hat, ev_net, sigma, market_price, s0 |
| `try_directional_entry()` at each early return | `filtered` | same + filter_reason |
| `close_position()` | `exit` | exit_price, exit_reason, pnl |

---

## 3. Three-Phase Execution Flow

```
Phase 1: Replay (offline, fast)
  ├── Consume tick feed → generate entry/exit/filtered signals
  ├── PgBacktestRecorder batch INSERT to backtest_signals + backtest_trades
  └── Write summary metrics to backtest_runs

Phase 2: Gamma Verification (online, optional)
  ├── Pull unique token_ids from backtest_trades
  ├── Check pm_token_settlements cache → only fetch missing from Gamma API
  ├── Compare: tick outcome (won) vs gamma settled_price
  ├── Update gamma_settled_price, gamma_resolved, gamma_match
  └── Print mismatches (if any)

Phase 3: Report (from DB)
  ├── Performance summary (PnL, win rate, Sharpe, drawdown, profit factor)
  ├── Calibration analysis (p_hat buckets vs actual win rate)
  ├── Missed opportunities (filtered signals by reason, avg EV)
  ├── Gamma verification summary (match rate, mismatches)
  ├── Profitability breakdown (by symbol, direction, time-of-day, EV bucket)
  ├── Fee impact analysis (gross PnL, fees, net PnL, fee drag %)
  ├── Optimization suggestions (rule-driven, with evidence)
  └── --json for external tool consumption
```

### CLI Interface

```bash
# Default: replay + gamma + report
ploy strategy backtest directional --symbols BTCUSDT

# Skip gamma (offline only, fast iteration)
ploy strategy backtest directional --symbols BTCUSDT --skip-gamma

# Verify an existing run with Gamma data
ploy strategy backtest directional --verify-run <run_id>

# List historical runs
ploy strategy backtest list

# Diff two runs
ploy strategy backtest diff <run_id_1> <run_id_2>
```

---

## 4. Report: Profitability & Optimization

### Profitability Breakdown

Five dimensions:

1. **By Symbol** — trades, PnL, win rate, profit factor per symbol
2. **By Direction** — UP vs DOWN performance split
3. **By Time-of-Day** — 8h buckets (Asia/EU/US sessions)
4. **By EV Bucket** — [0.10-0.15), [0.15-0.20), [0.20-0.30), [0.30+) with PF per bucket
5. **Fee Impact** — gross PnL, total fees, net PnL, fee drag percentage

### Optimization Suggestions (Rule-Driven)

Each suggestion includes: priority (HIGH/MED/LOW), evidence (data), estimated impact.

| Rule | Trigger | Suggestion |
|------|---------|------------|
| Low-EV bucket unprofitable | PF < 1.2 for lowest EV bucket | Raise `entry_threshold` |
| High-EV filtered signals | Filtered avg EV > 1.5× threshold, count > 20 | Relax that filter constraint |
| Weak symbol | Per-symbol PF < 1.15 | Drop symbol |
| Calibration overconfidence | p_hat > 0.70 bucket bias > 8% | Raise `vol_floor` |
| Fee drag excessive | Fee drag > 25% | Shift to higher-EV trades or limit 0.45-0.55 price range |

```rust
fn generate_suggestions(run: &BacktestRun, trades: &[Trade], signals: &[Signal]) -> Vec<Suggestion> {
    let mut suggestions = vec![];

    // 1. Low-EV bucket PF check
    let low_ev_pf = profit_factor(trades.iter().filter(|t| t.entry_ev_net < 0.15));
    if low_ev_pf < 1.2 {
        suggestions.push(Suggestion::raise_threshold(current, 0.15, low_ev_pf));
    }

    // 2. Filtered signals with high EV
    for (reason, group) in filtered_signals.group_by(|s| &s.filter_reason) {
        let avg_ev = group.iter().map(|s| s.ev_net).sum::<f64>() / group.len() as f64;
        if avg_ev > current_threshold * 1.5 && group.len() > 20 {
            suggestions.push(Suggestion::relax_filter(reason, avg_ev, group.len()));
        }
    }

    // 3. Per-symbol PF check
    for (symbol, group) in trades.group_by(|t| &t.symbol) {
        if profit_factor(&group) < 1.15 {
            suggestions.push(Suggestion::drop_symbol(symbol, profit_factor(&group)));
        }
    }

    // 4. Calibration bias in high-confidence bucket
    let high_bucket_bias = calibration_bias(trades, 0.70..1.0);
    if high_bucket_bias > 0.08 {
        suggestions.push(Suggestion::raise_vol_floor(high_bucket_bias));
    }

    suggestions.sort_by_priority();
    suggestions
}
```

---

## 5. Files to Create/Modify

### New Files
- `migrations/0XX_backtest_tables.sql` — Schema from §1
- `src/strategy/backtest_recorder.rs` — `BacktestRecorder` trait + `PgBacktestRecorder` + `NullRecorder`
- `src/strategy/backtest_report.rs` — Report generation, calibration, optimization suggestions

### Modified Files
- `src/strategy/directional_backtest.rs` — Add `recorder` field, call `record_*()` at entry/exit/filtered points
- `src/strategy/mod.rs` — Export new modules
- `src/cli/strategy.rs` — Wire up `--skip-gamma`, `--verify-run`, `backtest list`, `backtest diff` subcommands
