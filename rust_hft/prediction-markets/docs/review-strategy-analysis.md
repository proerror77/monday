# Ploy Strategy Analysis Report

**Date:** 2026-02-08
**Analyst:** Claude Opus 4.6 (Automated Strategy Review)
**Scope:** Core trading strategies, engine, risk management, and execution infrastructure
**Codebase:** ~74K lines Rust, 150+ source files

---

## Table of Contents

1. [Split Arbitrage (Core Strategy)](#1-split-arbitrage---core-strategy)
2. [Strategy Engine (State Machine)](#2-strategy-engine---state-machine)
3. [Momentum Strategy](#3-momentum-strategy)
4. [Signal Detection](#4-signal-detection)
5. [NBA Win Probability Model](#5-nba-win-probability-model)
6. [NBA Entry Logic](#6-nba-entry-logic)
7. [Multi-Event Monitor](#7-multi-event-monitor)
8. [Volatility Arbitrage](#8-volatility-arbitrage)
9. [Slippage Protection](#9-slippage-protection)
10. [Trading Costs](#10-trading-costs)
11. [Risk Management](#11-risk-management)
12. [Order Executor](#12-order-executor)
13. [Fund Manager](#13-fund-manager)
14. [Cross-Cutting Concerns](#14-cross-cutting-concerns)

---

## 1. Split Arbitrage - Core Strategy

**File:** `src/strategy/split_arb.rs` (861 lines)
**Priority:** Critical

### Logic Summary

The split arbitrage strategy exploits time-separated mispricings in Polymarket binary markets. The core insight is that retail panic creates mispricings at different times on opposite sides of a binary outcome:

1. Wait for the UP side to drop below a threshold (default 35 cents), buy UP.
2. Wait for the DOWN side to drop below a threshold, buy DOWN.
3. If `avg(UP) + avg(DOWN) < $0.99`, profit is locked because one side always settles at $1.00.

The engine monitors multiple series (SOL 15m, ETH 15m, BTC daily), tracks partial (unhedged) positions, and manages the hedge lifecycle with timeout and stop-loss protections.

### Identified Weaknesses

1. **No order fill confirmation for split arb orders.** The `execute_buy` and `execute_sell` methods fire-and-forget through the executor. Unlike the main engine which uses IOC/FOK with fill polling, split arb has no fill verification. The position is recorded as entered immediately after `execute()` returns, but the order may not have filled.

2. **Stale price cache with no TTL.** The `PriceCache` stores `(bid, ask, timestamp)` but never evicts stale entries. If WebSocket disconnects, the engine continues making decisions on arbitrarily old prices. The `timestamp` field is stored but never checked.

3. **No duplicate entry protection.** If two quote updates arrive in rapid succession for the same market, `check_new_entry` could fire twice before the first position is recorded (the write lock is acquired inside `check_new_entry` but the read in `check_opportunity` already released its lock).

4. **Hardcoded WebSocket URL.** The WebSocket endpoint `wss://ws-subscriptions-clob.polymarket.com/ws/market` is hardcoded in `run_split_arb`, making it impossible to configure for different environments.

5. **Exit at bid price assumes liquidity.** `exit_unhedged` uses `current_bid` as the sell price, but in a panic scenario the bid may be thin or absent. The fallback to `first_entry_price` when bid is unavailable is optimistic.

6. **No persistence of partial/hedged positions.** All state is in-memory. A crash loses all position tracking, potentially leaving orphaned positions on the exchange.

7. **Fixed shares per trade.** The `shares_per_trade` is static (default 100). There is no integration with the `FundManager` for dynamic position sizing based on available balance.

### Optimization Suggestions

- Add a staleness check to `PriceCache` (reject prices older than N seconds).
- Integrate fill confirmation (IOC with polling) into split arb order execution.
- Add an atomic "entering" flag per condition_id to prevent duplicate entries.
- Persist partial and hedged positions to PostgreSQL for crash recovery.
- Integrate `FundManager` for dynamic position sizing.
- Make the WebSocket URL configurable via `AppConfig`.

---

## 2. Strategy Engine - State Machine

**File:** `src/strategy/engine.rs` (1515 lines)
**Priority:** Critical

### Logic Summary

The `StrategyEngine` is a sophisticated state machine that orchestrates the two-leg arbitrage cycle for time-bounded binary markets (e.g., "Will BTC be above $X at time T?"). States flow as:

```
Idle -> WatchWindow -> Leg1Pending -> Leg1Filled -> Leg2Pending -> CycleComplete -> Idle
                                                                 -> Abort -> Idle
```

Key safety features:
- **Execution mutex** prevents concurrent order submissions.
- **Optimistic locking** (version counter) detects concurrent state modifications.
- **Forced Leg2** when approaching round end to avoid unhedged exposure.
- **Unwind mechanism** attempts to sell Leg1 exposure if Leg2 fails.
- **Circuit breaker** halts trading on consecutive failures or exposure anomalies.
- **IOC for Leg1, FOK for Leg2** ensures atomic execution semantics.

### Identified Weaknesses

1. **Version check duplication.** The version check pattern is repeated 6+ times with nearly identical code blocks (lines 604-631, 688-718, 1008-1035, 1074-1101). This is error-prone and makes maintenance difficult.

2. **Unwind is best-effort with no retry.** `unwind_leg1_exposure` makes a single IOC sell attempt. If it fails or partially fills, the remaining exposure is only logged, not retried. In a fast-moving market, a single attempt may be insufficient.

3. **No partial fill handling for Leg1.** If Leg1 IOC partially fills (e.g., 15 of 20 shares), the engine proceeds with `result.filled_shares` but the Leg2 FOK will attempt to match the partial fill count. If Leg2 also partially fills, the mismatch creates unbalanced exposure.

4. **`force_leg2_or_abort` uses REST prices.** When forcing Leg2 near round end, the engine fetches prices via REST (`get_prices`), which may be stale compared to the WebSocket feed. The forced price includes a slippage buffer but no depth check.

5. **State lock held across DB operations in `set_round`.** The write lock on `self.state` is held while calling `self.store.upsert_round()` (line 334), which is an async database operation. This blocks all other state reads during the DB call.

### Optimization Suggestions

- Extract the version-check-and-abort pattern into a helper method to reduce duplication.
- Implement a retry loop (2-3 attempts with backoff) for unwind operations.
- Consider allowing Leg2 to match partial Leg1 fills rather than requiring exact match.
- Use WebSocket cache prices for forced Leg2 when available, falling back to REST.
- Release the state lock before the DB upsert in `set_round`, re-acquiring afterward.

---

## 3. Momentum Strategy

**File:** `src/strategy/momentum.rs` (~1200 lines)
**Priority:** Critical

### Logic Summary

The momentum strategy exploits the lag between CEX (Binance) spot price movements and Polymarket binary option odds. It operates in two modes:

- **Confirmatory mode (CRYINGLITTLEBABY style):** Enters positions in the last 1-5 minutes of a 15-minute window when the CEX price has already moved decisively. Buys the winning side cheaply and holds to resolution to collect $1.00.
- **Predictive mode:** Enters earlier based on momentum signals, using take-profit/stop-loss/trailing-stop exits.

Key features include:
- Multi-timeframe momentum (10s/30s/60s weighted average)
- Volatility-adjusted entry thresholds (current vol / baseline vol)
- Order Book Imbalance (OBI) confirmation
- Price-to-beat fair value adjustment (how far CEX price is from the binary threshold)
- Time decay factor reducing signal strength as event progresses
- Dynamic position sizing based on confidence score
- Cross-symbol risk control (max $25 per 15-min window, best-edge-only selection)

### Identified Weaknesses

1. **Fair value model is heuristic, not calibrated.** The `estimate_fair_value` method uses a hand-tuned piecewise linear function (0.1% move = 5% prob shift, 0.5% = 20%, 1.0% = 35%). These mappings are not derived from historical data and may systematically over- or under-estimate edge.

2. **Confidence score uses `to_string().parse::<f64>()`.** The `calculate_confidence` method converts `Decimal` to `f64` via string parsing (line 914-917). This is both slow and fragile. A direct `.to_f64()` from `rust_decimal::prelude::ToPrimitive` would be cleaner and faster.

3. **`signal_collection_delay_ms` is configured but not clearly enforced in the main loop.** The `best_edge_only` and `signal_collection_delay_ms` fields exist in config but the actual signal queuing and selection logic is not visible in the detector -- it must be implemented in the runner, creating a separation of concerns issue.

4. **Hardcoded series ID mappings.** The `EventMatcher` hardcodes series IDs (e.g., "10192" for BTC 15m). If Polymarket changes series IDs, the strategy silently stops finding events.

5. **No backtest validation of the fair value model.** The sigmoid-like fair value estimation has no associated backtesting framework to validate its accuracy against historical outcomes.

6. **`max_window_exposure_usd` default is $25.** For a strategy that trades 4 symbols with 100 shares at 35 cents each ($35/trade), a single trade can exceed the window exposure limit. This creates a conflict between `shares_per_trade` and `max_window_exposure_usd`.

### Optimization Suggestions

- Calibrate the fair value model against historical 15-minute binary outcomes using logistic regression on CEX momentum features.
- Replace `to_string().parse::<f64>()` with `ToPrimitive::to_f64()` throughout.
- Fetch series IDs dynamically from the Gamma API rather than hardcoding them.
- Add a backtesting mode that replays historical CEX + PM data to validate edge estimates.
- Reconcile `shares_per_trade` with `max_window_exposure_usd` to prevent config conflicts.

---

## 4. Signal Detection

**File:** `src/strategy/signal.rs` (362 lines)
**Priority:** High

### Logic Summary

The `SignalDetector` identifies "dump" opportunities using a rolling price window. It tracks the maximum ask price over a configurable window (default 3 seconds) and fires a signal when the current ask drops by more than `move_pct` (default 15%) from the rolling high. Key behaviors:

- One signal per side per round (prevents duplicate triggers)
- Auto-resets when round slug changes
- Spread-based anti-fake-dump filter (signals include `spread_bps` for validation)
- Leg2 condition check: `leg1_price + opposite_ask <= effective_sum_target`

The effective sum target accounts for fees, slippage, and profit buffer:
`sum_target - fee_buffer - slippage_buffer - profit_buffer` (e.g., 0.95 - 0.005 - 0.02 - 0.01 = 0.915)

### Identified Weaknesses

1. **3-second rolling window is extremely short.** A 3-second window means the "rolling high" is essentially the highest price in the last 3 seconds. A brief spike followed by a return to normal could trigger a false dump signal. This is by design for fast markets but may generate noise.

2. **No volume confirmation.** The signal fires purely on price movement without checking if the price drop occurred on meaningful volume. A thin market with a single large ask removal could trigger a false signal.

3. **Spread filter is passive.** The spread is included in the signal but the detector itself does not reject wide-spread signals. Rejection happens in the engine (`signal.is_valid(max_spread_bps)`), creating a split responsibility.

4. **`check_leg2_condition` does not account for market impact.** The Leg2 check compares `leg1_price + opposite_ask` against the target, but the actual execution price may be worse than `opposite_ask` due to slippage on the order.

### Optimization Suggestions

- Add a configurable minimum window fill requirement (e.g., at least 3 price observations in the window before triggering).
- Include volume-weighted price tracking to filter out thin-market false signals.
- Move spread validation into the detector itself for cleaner separation.
- Add a slippage buffer to the Leg2 condition check to account for execution costs.

---

## 5. NBA Win Probability Model

**File:** `src/strategy/nba_winprob.rs` (365 lines)
**Priority:** High

### Logic Summary

A logistic regression model that predicts NBA game win probability from live game state. Features include point differential, time remaining, quarter indicators, possession, pregame spread, and Elo difference. The model includes interaction terms (`point_diff * time_remaining`, `point_diff * quarter_4`) to capture non-linear effects where score matters more late in the game.

Uncertainty is calculated heuristically based on time remaining (Q1 = 30%, Q4 = 5%), score extremity (blowouts add 15-25%), and pregame spread extremity.

### Identified Weaknesses

1. **Default model is untrained.** The `default_untrained()` method provides placeholder coefficients (e.g., `point_diff: 0.15`) that are explicitly marked as NOT trained on real data. If this default is used in production, predictions will be unreliable. There is no runtime guard preventing use of the untrained model.

2. **Uncertainty model is additive and uncalibrated.** Uncertainty components are simply summed (time + score + spread), capped at 0.5. This additive approach can produce unrealistic uncertainty values. For example, a Q1 blowout by 30 points with a 16-point spread would have uncertainty = 0.30 + 0.25 + 0.10 = 0.50 (maximum), even though the outcome may be quite predictable.

3. **No overtime handling.** The model has quarter indicators for Q1-Q4 but no handling for overtime periods. An overtime game would default to Q1 behavior (no quarter dummy active), producing incorrect predictions.

4. **`point_diff` coefficient of 0.15 is too high.** In the untrained default, each point of differential shifts the logit by 0.15, meaning a 10-point lead produces a logit shift of 1.5 (sigmoid ~ 0.82). In reality, a 10-point lead in Q1 is far less decisive than 82%.

5. **No feature normalization.** Features like `elo_diff` (range: -500 to +500) and `point_diff` (range: -40 to +40) operate on very different scales. Without normalization, the coefficient magnitudes are hard to interpret and tune.

### Optimization Suggestions

- Add a runtime check that rejects predictions from the untrained model (check `metadata.n_samples > 0`).
- Train the model on historical NBA play-by-play data (available from NBA API or basketball-reference).
- Add overtime quarter handling (Q5+) with appropriate coefficients.
- Implement isotonic calibration as a post-processing step.
- Normalize features to zero mean and unit variance before training.

---

## 6. NBA Entry Logic

**File:** `src/strategy/nba_entry.rs` (~350 lines)
**Priority:** High

### Logic Summary

The `EntryLogic` module implements a multi-stage entry decision pipeline for NBA markets:

1. **Market structure filters** (defensive gate -- rejects if filters fail)
2. **Price sanity checks** (min 5 cents, max 80 cents)
3. **Edge check** (`p_model - p_market >= min_edge`, default 5%)
4. **Confidence check** (model confidence >= 70%)
5. **Expected value calculation** (`gross_ev - fees - slippage >= min_ev_after_fees`, default 2%)

Each rejection includes a detailed reason and partial signal for analysis. Approved entries include full attribution (edge, fees, slippage, net EV, reasoning string).

### Identified Weaknesses

1. **Gross EV formula is incorrect.** The `calculate_gross_ev` method computes `p_model * 1.0 - p_market` (line 261). This is the expected profit per dollar of market price, but it conflates probability with price. The correct formula for a binary option is: `EV = p_model * (1 - p_market) - (1 - p_model) * p_market = p_model - p_market`. While the result is numerically the same as `edge`, the intermediate `p_model * 1.0` multiplication is misleading and the comment "payoff_if_win" suggests a misunderstanding of the payoff structure.

2. **Slippage is a fixed constant.** The `slippage_estimate` (default 0.5%) is a static config value, not derived from actual market depth. In illiquid NBA markets, actual slippage could be significantly higher.

3. **No Kelly criterion position sizing.** The config includes `max_position_pct` and `max_total_exposure_pct` but the entry logic does not compute Kelly-optimal position sizes. It only makes a binary approve/reject decision.

4. **Fee calculation ignores settlement.** `calculate_fees` only accounts for entry fees (`p_market * fee_rate`). Polymarket binary options that settle at $1.00 do not incur an exit fee, but early exits do. The model does not distinguish between hold-to-resolution and early-exit scenarios.

### Optimization Suggestions

- Implement Kelly criterion sizing: `f* = edge / (1 - p_market)` with fractional Kelly (e.g., quarter Kelly).
- Make slippage dynamic based on current order book depth from the quote cache.
- Add separate fee calculations for hold-to-resolution vs. early-exit scenarios.
- Add a "signal strength" tier system (strong/medium/weak) for position sizing.

---

## 7. Multi-Event Monitor

Historical note: the repo previously carried a `multi_event` monitor module
for series-wide arbitrage scanning. That module has since been retired during
legacy cleanup because it no longer had any runtime or CLI caller.

3. **No rate limiting on API refresh.** The `refresh_events` method calls `get_series_all_tokens` which hits the Polymarket API. There is no built-in cooldown; the caller must manage refresh frequency.

4. **Token-to-event mapping is never cleaned up for expired events.** When events are marked inactive, their token IDs remain in `token_to_event`, causing unnecessary lookups on stale tokens.

### Optimization Suggestions

- Add a per-event cooldown or "already signaled" flag to prevent duplicate opportunities.
- Log a warning and skip events with unparseable end dates rather than defaulting to `Utc::now()`.
- Clean up `token_to_event` entries when their corresponding events are removed.
- Add a `last_refresh` timestamp check to prevent excessive API calls.

---

## 8. Volatility Arbitrage

**File:** `src/strategy/volatility_arb.rs` (~500+ lines)
**Priority:** High

### Logic Summary

The volatility arbitrage strategy exploits mispricing in Polymarket 15-minute crypto binary options by comparing market-implied volatility with estimated realized volatility. The mathematical foundation uses binary option pricing:

```
P(YES) = N(d2), where d2 = buffer / (sigma * sqrt(T))
```

The strategy estimates realized volatility using a weighted blend of K-line historical volatility (70%) and tick-based volatility (30%). It then compares this against the implied volatility derived from market prices using Newton-Raphson iteration. When the volatility edge exceeds 15%, a trading signal is generated.

Key features:
- Abramowitz-Stegun normal CDF approximation
- Beasley-Springer-Moro inverse normal CDF
- Newton-Raphson implied volatility solver
- Quarter-Kelly position sizing with high-volatility regime detection
- Time window constraints (2-10 minutes, optimal 3-7 minutes)

### Identified Weaknesses

1. **Volatility estimation assumes stationarity.** The 70/30 K-line/tick blend assumes volatility is stable over the lookback period. In practice, crypto volatility exhibits clustering (GARCH effects). A sudden regime change (e.g., news event) would cause the historical estimate to lag significantly.

2. **Newton-Raphson implied vol solver has no convergence guarantee.** The solver iterates with a fixed step count and initial guess. For extreme market prices (near 0 or 1), the solver may not converge, returning `None`. The fallback of returning a default 0.3% for at-the-money cases is arbitrary.

3. **Binary option pricing model ignores drift.** The formula `P(YES) = N(buffer / (sigma * sqrt(T)))` assumes zero drift. Over 15-minute windows this is reasonable, but during trending markets the drift term can be material, especially for the 3-7 minute optimal window.

4. **High-vol Kelly multiplier is a blunt instrument.** The `high_vol_kelly_multiplier` (default 0.7) applies a flat reduction when combined volatility exceeds the threshold. A more nuanced approach would scale Kelly fraction continuously with volatility.

5. **No transaction cost adjustment in fair value.** The `calculate_fair_yes_price` function returns a theoretical fair value without deducting the 2% Polymarket fee. The fee deduction happens later in the signal generation, but the `min_price_edge` threshold (3%) must implicitly account for this.

### Optimization Suggestions

- Implement GARCH(1,1) or EWMA volatility estimation to capture volatility clustering.
- Add convergence checks and iteration limits with graceful fallback to the Newton-Raphson solver.
- Scale Kelly fraction continuously: `kelly * min(1.0, threshold / combined_vol)` instead of a binary multiplier.
- Incorporate a small drift term based on recent momentum when the buffer is small.
- Deduct fees directly in the fair value calculation for cleaner edge measurement.

---

## 9. Slippage Protection

**File:** `src/strategy/slippage.rs` (~250 lines)
**Priority:** High

### Logic Summary

The `SlippageProtection` module provides pre-trade slippage checks for both buy and sell orders. For each order it:

1. Checks market depth (requires `min_depth_multiple` times order size, default 5x).
2. Calculates slippage from a reference price (clamped to zero to prevent negative bypass).
3. Rejects orders exceeding `max_slippage_pct` (default 1%).
4. Approves with a recommended limit price (best ask/bid + 0.1% buffer).

### Identified Weaknesses

1. **Depth check uses only top-of-book size.** The `ask_size` and `bid_size` fields represent only the best level. A 5x depth multiple check against top-of-book is misleading -- the actual depth across multiple price levels may be much larger or smaller.

2. **Fixed 0.1% fill buffer is too small for volatile markets.** The limit price is set at `best_ask * 1.001` for buys. In fast-moving crypto markets, the ask can move more than 0.1% between quote and execution, causing unnecessary rejections.

3. **Sell-side slippage calculation has an asymmetry.** For sells, slippage is `(reference_price - best_bid) / reference_price`, clamped to zero. But the reference price for sells is typically the current bid, making slippage always zero unless the bid has moved since the reference was captured.

4. **No dynamic adjustment based on order urgency.** Forced Leg2 orders (near round end) should tolerate higher slippage than normal entries, but the slippage config is static.

### Optimization Suggestions

- Integrate full order book depth (multiple levels) when available from the WebSocket feed.
- Make the fill buffer configurable and scale it with recent volatility.
- Accept an `urgency` parameter that relaxes slippage limits for forced/unwind orders.
- Fix the sell-side slippage calculation to use the entry price as reference, not the current bid.

---

## 10. Trading Costs

**File:** `src/strategy/trading_costs.rs` (~200 lines)
**Priority:** Medium

### Logic Summary

The `TradingCostCalculator` provides comprehensive cost estimation for round-trip trades. It models four cost components:

1. **Entry fee** (maker/taker rate, default 0.2%)
2. **Exit fee** (maker/taker rate, default 0.2%)
3. **Gas costs** (2 transactions at $0.02 each = $0.04 round trip)
4. **Slippage** (quadratic model: `base_slippage + depth_ratio^2 * 1.6`, capped at tolerance)

### Identified Weaknesses

1. **Fee rate of 0.2% is incorrect for Polymarket.** Polymarket charges approximately 2% on the losing side of a binary option (not on both entry and exit). The current model charges 0.2% on both entry and exit notional, which underestimates costs for losing trades and overestimates for winning trades that settle at $1.00.

2. **Gas cost is static.** The $0.02 per transaction is hardcoded. Polygon gas costs can spike during network congestion, and the model does not account for this variability.

3. **Slippage model is not validated.** The quadratic slippage function (`0.001 + depth_ratio^2 * 1.6`) is a theoretical model with no empirical calibration against actual Polymarket execution data.

4. **No distinction between hold-to-resolution and early exit.** Binary options held to resolution have zero exit cost (settlement is automatic). The model always charges an exit fee, overstating costs for the primary hold-to-resolution strategy.

### Optimization Suggestions

- Update fee model to reflect Polymarket's actual fee structure (fee on losing side only).
- Add a `hold_to_resolution` flag that zeroes out exit fees and gas for settlement scenarios.
- Calibrate the slippage model against historical execution data (compare limit price vs. fill price).
- Query Polygon gas oracle for dynamic gas cost estimation.

---

## 11. Risk Management

**File:** `src/strategy/risk.rs` (~200 lines)
**Priority:** Critical

### Logic Summary

The `RiskManager` enforces trading limits through a three-tier risk state system:

- **Normal:** Trading allowed, all checks pass.
- **Elevated:** Trading allowed but with heightened monitoring (triggered at half the failure threshold).
- **Halted:** All trading stopped, requires manual reset.

Pre-trade checks include:
- Risk state validation (must be Normal or Elevated)
- Single exposure limit (`max_single_exposure_usd`)
- Minimum time remaining in round (`min_remaining_seconds`)
- Spread validation (anti-fake-dump)
- Forced Leg2 detection (`leg2_force_close_seconds`)

Post-trade tracking includes:
- Consecutive failure counter with circuit breaker (default threshold from config)
- Daily PnL tracking with loss limit enforcement
- Automatic state normalization after successful cycles

### Identified Weaknesses

1. **Daily PnL reset is implicit.** The `ensure_daily_reset` method (called inside `record_success` and `record_failure`) resets the daily PnL tracker when the date changes. However, if no trades occur on a new day, the stale previous-day PnL remains. This is benign but could cause confusion in monitoring.

2. **Single successful cycle immediately normalizes from Elevated.** After one success, the risk state drops from Elevated back to Normal (line 140-142). This is too aggressive -- a single lucky fill after multiple failures should not fully restore confidence. A gradual cooldown (e.g., require N consecutive successes) would be safer.

3. **No total exposure tracking across concurrent positions.** The risk manager checks single-position exposure but does not track aggregate exposure across all open positions. The `FundManager` handles this separately, creating a gap where the risk manager could approve a trade that violates aggregate limits.

4. **`record_loss` and `record_success` can race.** Both methods acquire the `daily_pnl` write lock, but `record_success` also checks the daily loss limit. If a loss and success are recorded concurrently, the PnL could be inconsistent.

### Optimization Suggestions

- Require N consecutive successes (e.g., 3) before normalizing from Elevated state.
- Add aggregate exposure tracking to the risk manager, or formalize the delegation to FundManager.
- Add a "warming up" state after circuit breaker reset that limits position sizes.
- Consider using a single atomic operation for PnL updates to prevent races.

---

## 12. Order Executor

**File:** `src/strategy/executor.rs` (~450 lines)
**Priority:** Critical

### Logic Summary

The `OrderExecutor` manages the full order lifecycle with retry logic, idempotency protection, and fill confirmation. Key behaviors:

- **Idempotency:** Generates a deterministic key per order request. Duplicate submissions return cached results or poll for completion.
- **Retry with exponential backoff:** Failed submissions retry up to `max_retries` with `100ms * 2^attempt` delay.
- **Fill confirmation:** For IOC/FOK orders, polls the exchange for fill status with configurable timeout. On timeout, attempts cancel + final status fetch.
- **Feishu notifications:** Sends trade alerts to Feishu (Lark) messenger on successful execution.
- **Dry run mode:** Simulates immediate fills at limit price.

### Identified Weaknesses

1. **Retry logic can create duplicate orders without idempotency.** When `idempotency` is `None` (the default constructor path), the executor calls `execute_with_retry` directly. If a submission succeeds but the response is lost (network timeout), the retry will submit a duplicate order. The safety guard in `StrategyEngine::new` (`confirm_fills` must be true for live trading) mitigates this, but the executor itself does not enforce it.

2. **Exponential backoff has no jitter.** The delay `100ms * 2^attempt` is deterministic. Under contention (multiple strategies retrying simultaneously), all retries will collide at the same times. Adding random jitter would reduce thundering-herd effects.

3. **`wait_for_fill` has no iteration limit.** The polling loop in `wait_for_fill` (line 360) runs indefinitely until the order reaches a terminal status. The caller wraps it in a `timeout`, but if the timeout is misconfigured (e.g., set to hours), the loop will spin indefinitely consuming API quota.

4. **Feishu notification is fire-and-forget.** The `notify_trade` call is awaited but errors are silently ignored. If Feishu is down, the notification blocks the execution path for the full HTTP timeout before proceeding.

### Optimization Suggestions

- Add jitter to exponential backoff: `delay * (0.5 + random(0, 1))`.
- Add a maximum iteration count to `wait_for_fill` as a safety net.
- Make Feishu notification non-blocking by spawning it as a background task.
- Enforce idempotency as mandatory for live trading (not optional).

---

## 13. Fund Manager

**File:** `src/strategy/fund_manager.rs` (~400 lines)
**Priority:** High

### Logic Summary

The `FundManager` provides centralized position sizing and balance management. Key capabilities:

- **Dynamic per-symbol allocation:** Divides available balance equally across configured symbols (default 4: BTC, ETH, SOL, XRP).
- **Multi-layer position approval:** Checks max positions, per-symbol limits, balance minimums, per-symbol allocation, and Polymarket minimums (5 shares, $1 order value).
- **TTL-based balance caching:** Caches USDC balance for 10 seconds to reduce API calls.
- **Exposure tracking:** Tracks deployed funds per symbol and per event, with open/close recording.

### Identified Weaknesses

1. **Equal allocation across symbols ignores volatility differences.** The fund manager divides available balance equally across all symbols (`available / total_symbols`). BTC and SOL have very different volatility profiles, and equal allocation means the portfolio is implicitly overweight on higher-volatility assets in risk terms.

2. **Exposure tracking is in-memory only.** If the process crashes, all position tracking is lost. The `active_positions`, `positions_per_symbol`, and `symbol_exposure` maps are not persisted. On restart, the fund manager thinks no positions exist, potentially allowing over-allocation.

3. **`record_position_opened` with zero amount is misleading.** The convenience method `record_position_opened(event_id, symbol)` calls `record_position_opened_with_amount` with `Decimal::ZERO`. This means the exposure tracking shows $0 for positions opened via this path, understating actual exposure.

4. **Balance cache invalidation on position close is aggressive.** Every position open/close invalidates the balance cache, forcing a fresh API call on the next check. With multiple concurrent positions closing, this creates a burst of API calls.

5. **No integration with the split arb engine.** The `SplitArbEngine` uses a fixed `shares_per_trade` and does not consult the `FundManager` at all, creating a parallel position sizing path that bypasses all fund management controls.

### Optimization Suggestions

- Implement volatility-weighted allocation: allocate more capital to lower-volatility symbols.
- Persist position tracking to PostgreSQL for crash recovery.
- Always pass the actual USD amount to `record_position_opened_with_amount`.
- Batch cache invalidation: delay re-fetch by 1-2 seconds after position changes to coalesce multiple events.
- Require all strategy engines (including split arb) to route through FundManager.

---

## 14. Cross-Cutting Concerns

### Priority Summary Table

| # | Strategy/Module | Priority | Key Issue |
|---|----------------|----------|-----------|
| 1 | Split Arbitrage | Critical | No fill confirmation, no crash recovery |
| 2 | Strategy Engine | Critical | Version check duplication, single-attempt unwind |
| 3 | Momentum | Critical | Uncalibrated fair value model |
| 4 | Signal Detection | High | 3-second window noise, no volume filter |
| 5 | NBA Win Prob | High | Untrained default model used in production path |
| 6 | NBA Entry | High | Incorrect gross EV formula, static slippage |
| 7 | Multi-Event | Medium | No dedup, stale token mappings |
| 8 | Volatility Arb | High | Stationary vol assumption, solver convergence |
| 9 | Slippage | High | Top-of-book only, no urgency scaling |
| 10 | Trading Costs | Medium | Wrong fee model for Polymarket |
| 11 | Risk Management | Critical | Single success normalizes, no aggregate exposure |
| 12 | Order Executor | Critical | No jitter on retry, unbounded poll loop |
| 13 | Fund Manager | High | In-memory only, no split arb integration |

### Architectural Observations

**Strength: Defense-in-depth for the engine.** The `StrategyEngine` has multiple overlapping safety mechanisms -- execution mutex, optimistic locking, circuit breaker, forced Leg2, unwind, and DB persistence. Any single failure is caught by at least one other layer.

**Strength: Clean state machine design.** The `StrategyState` enum with explicit transitions (`Idle -> WatchWindow -> Leg1Pending -> ...`) makes the engine's behavior predictable and auditable. The `requires_abort_on_round_end()` method cleanly separates states that need emergency handling.

**Strength: Multi-strategy diversity.** The codebase supports four distinct edge sources (split arb, momentum, NBA win prob, volatility arb) across three domains (crypto, sports, politics). This diversification reduces correlation risk.

**Weakness: Strategy isolation.** Each strategy (split arb, momentum, volatility arb) has its own position tracking, risk checks, and execution path. There is no unified portfolio-level risk view. If all three strategies enter positions simultaneously, aggregate exposure could exceed intended limits.

**Weakness: No unified backtesting framework.** Each strategy has ad-hoc testing but no shared backtesting infrastructure. The `backtest.rs` and `execution_sim.rs` files exist but are not integrated with the newer strategies (momentum, volatility arb).

**Weakness: Configuration sprawl.** There are at least 6 separate config structs (`SplitArbConfig`, `MomentumConfig`, `ExitConfig`, `VolatilityArbConfig`, `SlippageConfig`, `TradingCostConfig`) with overlapping parameters (e.g., slippage appears in 3 places). A unified config hierarchy would reduce inconsistencies.
