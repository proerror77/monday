# Trailing Exit for DirectionalStrategy

**Date:** 2026-04-16
**Branch:** feat/runtime-ownership-refactor
**Scope:** `crates/ploy-strategy-bundles/src/strategies/directional.rs`

## Problem

`DirectionalStrategy` currently has two exit paths:

1. Settlement (market expiry) — `build_settlement_exits`
2. Signal reversal (new opposing signal)

Three config fields — `take_profit_price_delta`, `stop_loss_price_delta`, `max_hold_secs` — exist in `DirectionalConfig` but are never read anywhere in the strategy logic. Positions ride to settlement regardless of mid-trade price movement, leaving unrealized gains on the table when PM prices spike and then reverse.

This mirrors the "Rescue 4" pattern from the HyperLiquid strategy audit: entry logic has edge, exit logic is absent.

## Goal

Add a tick-driven exit that combines:
- **Fixed take-profit**: exit when current ask ≥ entry + `take_profit_price_delta`
- **Trailing stop**: exit when current ask ≤ peak − `trailing_stop_delta`
- **Timeout**: exit when hold duration ≥ `max_hold_secs` (backstop)

All three checks run on every `MarketUpdate::Quote` for tokens with open positions. No new runtime loop or timer required.

## Data Structure

Replace `entry_prices: HashMap<String, Decimal>` with `positions: HashMap<String, PositionTracker>`:

```rust
struct PositionTracker {
    entry_price: Decimal,      // ask at fill time
    entry_time: DateTime<Utc>, // fill timestamp
    peak_ask: Decimal,         // highest ask seen since entry
}
```

`peak_ask` is initialized to `fill.price` on Buy fill, then updated on every Quote tick for that token.

## Exit Logic (Quote tick)

```
if ask >= entry_price + take_profit_price_delta  → fixed take-profit exit
else if ask <= peak_ask - trailing_stop_delta    → trailing stop exit
else if (now - entry_time).secs >= max_hold_secs → timeout exit
```

Priority: fixed take-profit > trailing stop > timeout. Only one Exit intent per token per tick.

Exit `limit_price`: use current `bid` when available, `None` (market) otherwise — consistent with settlement exits.

## Config Changes

| Field | Status | Default | Notes |
|---|---|---|---|
| `take_profit_price_delta` | existing (was dead) | 0.10 | fixed TP distance in ask price units |
| `max_hold_secs` | existing (was dead) | 120 | timeout backstop |
| `trailing_stop_delta` | **new** | 0.05 | trailing drawdown from peak |

`stop_loss_price_delta` remains in config for backwards-compat but stays unused (its semantic is now covered by `trailing_stop_delta`).

## Scope Boundaries

- Only applies to main V3 path (up/down tokens of active events).
- Reversal (V5) path is not touched; its own `reversal_take_profit_ask` / `reversal_stop_distance_pct` remain dead code for now.
- `peak_ask` depends only on feed Quote data — no `Utc::now()` — so backtest replay is fully deterministic.
- `PositionTracker` replaces `entry_prices` at all three existing call sites: Buy fill insert, Sell fill remove + PnL calc, settlement PnL approximation.

## Files Changed

- `crates/ploy-strategy-bundles/src/strategies/directional.rs` — only file touched

## Out of Scope

- Reversal exit activation
- Per-symbol trailing stop overrides (can be added later via `symbol_profiles`)
- Fee-adjusted exit thresholds
