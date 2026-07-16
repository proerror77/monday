# Liquidity Vacuum Strategy Specification (v0.1)

## 1. Purpose

This strategy does not predict event outcomes directly. It trades the forced behavior of market participants during panic moves.

Core idea:

1. Detect a short-term liquidity vacuum where flow is one-sided and crowded.
2. Estimate a baseline expected price from the CEX anchor.
3. Enter in the opposite direction of crowd panic when dislocation is extreme.
4. Exit on mean reversion to EMA band or hard stop loss.

This document is decision-complete for implementation in `StrategyManager`.

## 2. Scope and Runtime

1. Product scope: Polymarket binary up/down markets mapped to one CEX anchor symbol (default `BTCUSDT`).
2. Runtime scope: event-driven strategy loop using CEX ticks + Polymarket order flow + order book snapshots.
3. Timeframe: rolling 90-second signal window.
4. Current phase: spec + config template only; code implementation follows this spec.

## 3. Data Contract

## 3.1 Required Inputs

1. CEX last trade price stream for anchor symbol.
2. CEX trade volume stream for anchor symbol.
3. Polymarket executed flow (buy/sell direction per trade).
4. Polymarket order book depth snapshots (top N levels minimum).
5. Event metadata and token mapping for corresponding up/down market.

## 3.2 Time Alignment Rules

1. All timestamps must be normalized to UTC.
2. Clock skew tolerance across feeds: `<= 500ms`.
3. Any feature using stale input older than `max_quote_age_ms` is invalid for entry.
4. Missing required feed in current 90-second window forces `NO_TRADE`.

## 4. Feature Definitions

All features are computed on rolling window `W = 90s`.

1. `price_move_90s = abs(P_now - P_90s_ago) / P_90s_ago`
2. `volume_90s = sum(CEX trade size over W)`
3. `volume_baseline_90s = rolling mean of volume_90s over recent baseline window`
4. `volume_ratio = volume_90s / volume_baseline_90s`
5. `net_flow_signed_90s = (buy_volume - sell_volume) / (buy_volume + sell_volume)` on PM flow, range `[-1, 1]`
6. `book_skew_90s = (bid_depth - ask_depth) / (bid_depth + ask_depth)` averaged over W, range `[-1, 1]`
7. `crowd_vote = 0.7 * net_flow_signed_90s + 0.3 * book_skew_90s`
8. `EMA_200`: 200-period EMA on CEX anchor price stream
9. `expected_price = EMA_200 * (1 + sentiment_offset)`
10. `deviation = abs(P_now - expected_price) / expected_price`

Defaults:

1. `sentiment_offset = 0.0` unless symbol-specific override is configured.
2. If denominator is zero or invalid, mark feature invalid and skip entry.

## 5. Trigger Conditions (All Required)

Entry trigger is valid only when all conditions are true:

1. `price_move_90s > 0.02`
2. `volume_ratio > 3.0`
3. `abs(crowd_vote) >= 0.70`
4. `deviation > 0.12`

If any condition is false, strategy must not open a position.

## 6. Direction Logic

Crowd direction:

1. `crowd_vote > 0` means crowd is net chasing up.
2. `crowd_vote < 0` means crowd is net chasing down.

Contrarian entry direction:

1. If `crowd_vote > 0`, open `NO`.
2. If `crowd_vote < 0`, open `YES`.

No-trade guard:

1. If `abs(crowd_vote) < 0.70`, no entry.
2. If direction signals conflict due to missing flow/depth components, no entry.

## 7. Entry and Exit Rules

## 7.1 Entry

1. Entry is allowed only after all trigger conditions pass.
2. Use liquidity and spread guardrails before submitting order.
3. Enforce cooldown and max concurrent positions.

## 7.2 Stop Loss

1. Hard stop loss at `25%` adverse move from entry on contract mark price.
2. Formula:
   - `pnl_pct = (mark_price - entry_price) / entry_price` for long token.
   - Exit when `pnl_pct <= -0.25`.

## 7.3 Take Profit

Take-profit is EMA-band based and fully exits position:

1. For contrarian `NO` opened during upward panic:
   - Exit when CEX anchor re-enters upper EMA band: `P_now <= EMA_200 * 1.03`.
2. For contrarian `YES` opened during downward panic:
   - Exit when CEX anchor re-enters lower EMA band: `P_now >= EMA_200 * 0.97`.

If both TP and SL would trigger on same tick, SL takes priority.

## 7.4 Forced Exit

1. Force-close before market resolution by configured buffer (`force_exit_before_resolution_secs`).
2. Force-close on feed integrity failures exceeding tolerance.

## 8. Risk Controls

1. `max_positions`: cap concurrent positions.
2. `max_daily_trades`: cap daily trade count.
3. `cooldown_secs`: min delay between entries on same symbol.
4. `max_notional_usd`: strategy-level exposure cap.
5. `min_liquidity_usd`: reject entries when order book depth is insufficient.
6. `max_spread_bps`: reject entries in wide-spread regimes.

## 9. Replay and Validation Protocol

Historical performance numbers (`1847 trades`, `73.2% win rate`, `Sharpe 2.7`) are not treated as accepted truth until replay verification completes.

Required replay outputs:

1. Total trades.
2. Win rate.
3. Sharpe ratio.
4. Profit factor.
5. Max drawdown.
6. Exposure utilization and rejection reason breakdown.

Acceptance criteria for implementation phase:

1. Strategy reproduces deterministic entries from same replay input.
2. Trigger boundaries behave exactly at thresholds.
3. Exit precedence (`SL > TP > forced`) is deterministic.
4. Replay report includes parameter hash and dataset hash.

## 10. Edge Cases and Failure Modes

1. Missing CEX price in window: no entry, keep managing existing positions.
2. Missing PM flow/depth in window: no entry.
3. Invalid EMA due to insufficient warmup: no entry until warm.
4. Quote freeze / stale data: block new entries, allow risk exits.
5. Resolution too near: block entries, only manage exits.

## 11. Implementation Notes (for follow-up coding)

1. Planned strategy id: `liquidity_vacuum`.
2. Planned config path: `config/strategies/liquidity_vacuum.toml`.
3. Planned integration points:
   - `src/strategy/manager.rs` (`StrategyFactory::from_toml`, `available_strategies`)
   - new adapter/strategy module under `src/strategy/`
   - existing execution and risk checks reused from current framework
