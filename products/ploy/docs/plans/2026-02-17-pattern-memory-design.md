# Pattern Memory (Bayesian) Strategy Design

**Date:** 2026-02-17

## Goal

Implement a new strategy `pattern_memory` that trades Polymarket crypto `up-or-down-5m` markets using:

- Binance **Kline WebSocket** closed bars (`5m` + `15m`) for feature generation
- Similarity search over recent-return patterns (no neural nets)
- **Bayesian posterior** probability of resolving **above** `price_to_beat` (Objective B)
- Multi-timeframe agreement: `5m` signal must agree with `15m` filter

The model output is not “predict the future” in a vacuum; it estimates:

> Given the current pattern and the required return to finish above the threshold, what is the posterior probability of finishing above the threshold at the next 5-minute boundary?

## Key Definitions

### Time Alignment (5m)

At each closed 5m bar time `t` (a 5-minute boundary):

1. We compute the current 5m pattern `P(t)` using the last `N` close-to-close returns ending at `t`.
2. We select the Polymarket 5m event that is **starting now** (time remaining near 300s, ends at `t + 5m`).
3. We compute the required return to finish above threshold:
   - `r_req = (price_to_beat - spot_price) / spot_price`
4. We estimate posterior `p_up(t)` that the next 5m return will exceed `r_req`.
5. If `5m` direction agrees with `15m` direction and EV thresholds pass, we place an order on the corresponding UP/DOWN token.

This avoids variable-horizon prediction. We only trade at event boundaries.

### Pattern

- Pattern length: `N = 10`
- Inputs: close-to-close **returns** (percent changes), scale-invariant by construction

For timeframe `tf` and symbol `S`, the sample at time `t` stores:

- `pattern`: last `N` returns ending at `t`
- `next_return`: return from `t` to `t + tf` (known one bar later)

### Similarity

Pearson correlation between current pattern and historical patterns:

- Match set `M = { i | corr_i >= thr }`, default `thr = 0.70`

### Weighted Bayesian Posterior (Beta-Binomial)

For each match `i ∈ M`, compute weight:

- `w_i = clamp((corr_i - thr) / (1 - thr), 0..1)`

Convert match outcomes using the **current event’s required return**:

- For UP: match is “success” if `next_return_i > r_req`
- Otherwise “failure” (including equality)

Aggregate:

- `up_w = Σ w_i * 1[next_return_i > r_req]`
- `down_w = Σ w_i * 1[next_return_i <= r_req]`
- `n_eff = up_w + down_w`

With prior `Beta(α, β)` (default `α=β=1`):

- `p_up = (α + up_w) / (α + β + n_eff)`

Direction:

- `dir_5m = UP` if `p_up >= 0.5`, else `DOWN`
- `conf_dir = max(p_up, 1 - p_up)`

The posterior naturally shrinks toward 0.5 when `n_eff` is small. We still enforce a minimum `n_eff` gate.

### 15m Filter

15m engine produces a direction filter independent of `price_to_beat`:

- Use `r_req = 0` for 15m, i.e., success if `next_return_15m > 0`
- Compute posterior as above to obtain `dir_15m`

Trade gate:

- Only trade when `dir_5m == dir_15m`

## Trading Rules (Polymarket)

### Event Selection

For each symbol, maintain discovered 5m events (via Gamma series IDs).

Select event to trade at time `t`:

- Must have `price_to_beat` parsed from title/question
- `time_remaining_secs ∈ [min_time_remaining, max_time_remaining]` (default `[240, 360]`)
- Prefer event with time remaining closest to 300s

### Quotes and Entry

Use Polymarket best ask of the chosen side token as entry price.

Compute expected value using fee-adjusted EV (fee defaults to 2%):

- Use posterior as true probability: `p_true = p_up` for UP, `1 - p_up` for DOWN
- Gate by:
  - `entry_price <= max_entry_price`
  - `n_eff >= min_effective_samples`
  - `conf_dir >= min_confidence`
  - `EV_net > 0` and/or `edge >= min_edge`

### Risk Controls (v1)

- One entry per event (`event_id` de-dup)
- Max concurrent positions
- Cooldown per symbol
- v1 exits: hold to resolution (no early TP/SL)

## Data Feeds and Components

### Binance

Add `BinanceKlineWebSocket` adapter:

- Streams: `kline_5m` and `kline_15m` for configured symbols
- Only forward `k.x == true` (closed bars)
- Reconnect w/ backoff, ping/pong, optional proxy support

### Polymarket

Use Gamma series discovery via `PolymarketClient.get_all_active_events(series_id)`:

- Extract UP/DOWN token IDs from `GammaEventInfo.markets[*].clobTokenIds` (fallback to `tokens`)
- Parse `price_to_beat` from `title` / `question`

Subscriptions:

- Maintain desired token set for current/near-future events
- Update `PolymarketWebSocket` token mapping via `reconcile_token_sides`
- Call `request_resubscribe()` when token set changes

## Backtest / Evaluation (A)

We cannot reconstruct historical Polymarket thresholds reliably from Binance alone.

v1 evaluation plan:

- Offline: build memory from Binance 5m/15m klines; evaluate calibration/accuracy for a configurable set of synthetic `r_req` offsets (e.g., bps grid).
- Online paper mode: log real Polymarket thresholds + posterior + realized outcome (from next 5m close), then compute post-hoc metrics.

## Testing Strategy

- Unit tests for:
  - Pearson correlation & degeneracy handling
  - Weight function and clamping
  - Beta posterior math
  - `r_req` classification (`next_return > r_req` strictness)
  - Pattern sample lifecycle: pending sample becomes stored on next bar
  - Binance kline JSON parsing (closed-bar detection)

Integration checks (manual):

- Run in `--dry-run` for 30+ minutes and verify:
  - Kline events flow for both intervals
  - Polymarket event discovery rotates and resubscribes tokens
  - Strategy produces signals and logs EV gates
