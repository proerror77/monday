# pm_5m_directional v2 — Three-Route Alpha Discovery

**Date**: 2026-03-14
**Status**: Approved
**Goal**: Find real edge for the 5-minute crypto directional strategy by testing
three independent signal improvements in parallel.

## Problem Statement

The current `pm_5m_directional` strategy has structural weaknesses:

1. **tau_scale is wrong** — uses `remaining/300` instead of annualized time,
   systematically biasing z-scores
2. **30s realized vol is noisy** — single spikes cause wild p_hat swings
3. **Binance OBI/flow is lagging** — by the time we see confirmation, PM makers
   have already adjusted
4. **No early exit** — hold to expiry with no stop-loss
5. **Bayesian settlement uses stale quotes** — `current_price > 0.5` is not
   actual payout

## Three Routes

### Route A — Fix Existing Binance Signal

Smallest change, fastest to backtest. Fix the math and add exit logic.

**Changes:**

1. **tau_scale correction**
   ```
   // Before (wrong): tau_scale = remaining_secs / 300.0
   // After (correct): tau_years = remaining_secs / (365.25 * 24 * 3600)
   // z = ln(S/K) / (sigma_annual * sqrt(tau_years))
   ```

2. **EWMA volatility (λ=0.94)**
   - Per-second log returns, exponentially weighted
   - `ewma_var = λ * ewma_var + (1-λ) * r²`
   - Annualize: `sigma = sqrt(ewma_var * 365.25 * 24 * 3600)`
   - Smoother than 30s window, faster reaction than simple average

3. **Early exit**
   - Binance price reversal > `exit_reversion_pct` (default 0.3%)
   - OBI sign flip (was positive, now negative for UP position)
   - Exit via IOC sell at best bid on PM

4. **Bayesian settlement fix**
   - Track event resolution status from `EventExpired` updates
   - Use actual payout (1.0 or 0.0) instead of last quote

**Files:** Modify `pm_5m_directional.rs` in-place (or fork to `_v2.rs`)
**Backtest:** Existing Binance + PM data. Can run immediately.

### Route B — Deribit IV as Regime Filter

Deribit options IV doesn't match the 5-minute horizon for direction, but it's
useful as a **regime classifier** to adjust position sizing.

**Signal design:**

- Subscribe to Deribit ATM options ticker (nearest daily expiry)
- Compute ATM IV for BTC and ETH
- Classify regime:
  - `LowVol`: ATM IV < 40% annualized → full position size
  - `NormalVol`: 40-80% → normal size
  - `HighVol`: > 80% → reduce to 50% size
  - `TermInversion`: near IV > far IV → pause trading (expect event)

**Rationale:** In high-vol regimes, 5-minute binary options approach coin-flip
territory (p_hat ≈ 0.50 for any strike). Reducing size preserves capital for
better conditions.

**Files:**
- New: `src/adapters/deribit_ws.rs` — WS client for Deribit public ticker
- New: `DataFeed::DeribitIV`, `MarketUpdate::DeribitIV` in traits.rs
- Modify: strategy to consume regime state and scale position size

**Backtest:** Needs Deribit historical IV data. Options:
- Daily-level from CryptoDataDownload (coarse but free)
- Collect live data for 2-3 weeks before proper backtest
- Use DVOL index as proxy (Deribit publishes 30-day IV index)

### Route C — Binance Perp Funding + Liquidation

Most time-horizon-matched new signal source. Perpetual funding rate and
liquidation cascades directly reflect short-term directional pressure.

**Signal design:**

1. **Funding rate** (Binance `@markPrice` stream, updates every second)
   - Extreme positive (> 0.01%) → market overleveraged long → bearish bias
   - Extreme negative (< -0.01%) → overleveraged short → bullish bias
   - Use as contrarian signal: fade the crowd

2. **Liquidation stream** (`forceOrder` WS stream)
   - Track rolling 2-minute liquidation volume by side
   - Large long liquidations → downward cascade likely continuing
   - Large short liquidations → upward cascade
   - Use as momentum signal: follow the cascade

3. **Composite entry:**
   ```
   direction = z_score direction (existing)
   confirmed = funding_signal agrees OR liquidation_signal agrees
   entry if: effective_p >= p_entry AND confirmed AND edge >= min_edge
   ```

**Files:**
- Extend: `src/adapters/binance_ws.rs` — add funding rate + liquidation subs
- New: `MarketUpdate::BinanceFunding`, `MarketUpdate::BinanceLiquidation`
- Modify: strategy to consume new signals

**Backtest:**
- Funding rate: Binance REST API has historical data (`/fapi/v1/fundingRate`)
- Liquidation: no public historical API, must collect going forward
- Can backtest funding-only signal immediately, add liquidation later

## Shared Infrastructure

### Backtest Comparison Framework

All three routes use the same backtest harness and PM event history. Compare:

| Metric         | Description                              |
|----------------|------------------------------------------|
| Win rate       | Percentage of profitable trades          |
| Avg edge       | Mean profit per trade after fees         |
| Max drawdown   | Worst peak-to-trough decline             |
| Sharpe ratio   | Annualized risk-adjusted return          |
| Trade count    | Total trades (< 30 = statistically weak) |
| Profit factor  | Gross profit / gross loss                |

### Implementation Order

1. **Route A first** — no new adapters needed, can backtest today
2. **Route C second** — funding rate data available, extend existing adapter
3. **Route B last** — needs new adapter + data collection period

### File Layout

```
src/strategy/pm_5m_directional.rs      — Route A (modify in place)
src/strategy/pm_5m_directional_v2.rs   — Combined v2 with all routes
src/adapters/deribit_ws.rs             — Route B adapter
src/adapters/binance_ws.rs             — Route C (extend)
src/strategy/traits.rs                 — New MarketUpdate variants
config/strategies/pm_5m_directional_v2_default.toml
```

## Success Criteria

A route is worth keeping if it shows, over at least 50 trades in backtest:
- Win rate > 60%
- Avg edge > 2% after fees
- Sharpe > 1.5
- Profit factor > 1.5

Routes that don't meet these thresholds get dropped. Routes that do get merged
into the final v2 strategy.
