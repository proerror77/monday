# PM5D Reversal Strategy — Design Spec

**Date:** 2026-04-13
**Status:** Draft
**Scope:** Full-stack redesign — alpha hypothesis → attribution tooling → ReversalStrategy implementation

---

## 1. Alpha Hypothesis

Polymarket 5-minute binary markets (UP/DOWN) price in CEX spot direction with a lag.
When CEX spot momentum **reverses** near the `price_to_beat` threshold, the PM market
is slow to reprice — creating a window where the reversing token (UP or DOWN) is
underpriced relative to the true probability of crossing `price_to_beat`.

**The edge is the lag between CEX momentum reversal and PM quote adjustment.**

### Entry Conditions (conceptual)

```
1. POSITION:   |spot - price_to_beat| / price_to_beat < threshold
               → spot is close enough that a reversal can still cross the line

2. REVERSAL:   drift_speed sign flips (was negative, now positive, or vice versa)
               AND drift has persisted in new direction for N seconds
               → momentum has genuinely turned, not just noise

3. LOB SUPPORT: bid_depth > ask_depth at price levels near spot (for UP reversal)
               OR ask_depth > bid_depth (for DOWN reversal)
               → order book confirms the reversal direction

4. PM LAG:     token_ask < fair_value_estimate
               → PM has not yet priced in the reversal

5. TIME:       time_remaining > min_secs AND time_remaining < max_secs
               → enough time for spot to cross price_to_beat
```

### Why This Is Different From Current DirectionalStrategy

| | DirectionalStrategy | ReversalStrategy |
|---|---|---|
| Signal | Trend continuation | Trend reversal |
| S0 usage | `price_to_beat` as log-normal anchor | `price_to_beat` as crossing target |
| Probability model | Log-normal P(S_T ≥ S_0) | Distance + velocity + time model |
| LOB usage | OBI as soft multiplier | Price-level support/resistance as hard gate |
| PM ask role | Entry cost filter | Mispricing detector |

---

## 2. Phase 1 — Signal Attribution Tool

**Goal:** Before writing any strategy logic, build a tool that answers:
- Do reversal signals actually predict outcomes?
- How much does LOB support add?
- How long does PM lag last after a reversal?
- Which symbols have the strongest reversal alpha?

### 2.1 New Example: `signal_attribution`

**Location:** `crates/ploy-strategy-bundles/examples/signal_attribution.rs`

**What it does:** Replays historical data and, for every 5m event window, records a
row of features at each spot tick — not just at entry. This produces a dataset of
`(features_at_t, outcome)` pairs that can be analyzed offline.

**Output schema (CSV or JSONL):**

```
event_id, symbol, window_start, window_end, price_to_beat,
tick_ts, time_remaining_secs,
spot_price, distance_to_beat_pct,
drift_speed_30s, drift_speed_10s, drift_direction_flipped,
drift_flip_age_secs,
obi_5, obi_delta, spread_bps,
lob_bid_depth_near, lob_ask_depth_near,
pm_up_ask, pm_down_ask,
pm_ask_lag_secs,
outcome_up_won
```

**Key fields explained:**

- `distance_to_beat_pct`: `(spot - price_to_beat) / price_to_beat` — signed, negative means below
- `drift_direction_flipped`: boolean, true if drift sign changed in last 30s
- `drift_flip_age_secs`: how many seconds ago the flip happened
- `lob_bid_depth_near` / `lob_ask_depth_near`: total volume within 0.1% of spot from `bids`/`asks` JSONB
- `pm_ask_lag_secs`: seconds since last PM quote update for this token
- `outcome_up_won`: from `pm_token_settlements`, official result

**Usage:**

```bash
cargo run --release -p ploy-strategy-bundles --example signal_attribution -- \
  --db-url postgresql://... \
  --start-date 2026-04-01 \
  --end-date 2026-04-10 \
  --symbols BTCUSDT,DOGEUSDT \
  --output /tmp/attribution.csv
```

### 2.2 LOB Depth Parsing

`binance_lob_ticks.bids` and `.asks` are stored as JSONB arrays of `[price, quantity]` pairs.
The database loader needs a new helper to extract near-spot depth:

```rust
fn near_depth(levels: &serde_json::Value, spot: f64, pct_range: f64) -> (f64, f64) {
    // Returns (bid_volume_near, ask_volume_near) within pct_range of spot
}
```

This is additive — existing `MarketUpdate::L2` keeps `obi_5` and `spread_bps`.
A new `MarketUpdate::L2Depth` variant carries the parsed near-depth values for
use by strategies that need it.

### 2.3 What We Learn From Phase 1

Before writing Phase 2, run the attribution tool and answer:

1. **Reversal predictiveness:** When `drift_direction_flipped=true` and `drift_flip_age_secs < 15`,
   what is the win rate by symbol?
2. **Distance filter:** What `distance_to_beat_pct` range has the best win rate?
3. **LOB confirmation:** Does `lob_bid_depth_near > lob_ask_depth_near` improve win rate?
4. **PM lag window:** How long after a reversal does PM ask stay below fair value?
5. **Time window:** What `time_remaining_secs` range has the best outcome rate?

These answers directly set the gate thresholds in Phase 2.

---

## 3. Phase 2 — ReversalStrategy

**Location:** `crates/ploy-strategy-bundles/src/strategies/reversal.rs`

**Trait:** `impl StrategyLogic for ReversalStrategy` — plugs into the existing
`StrategyRuntime<S, F, E>` without any runtime changes.

### 3.1 State

```rust
pub struct ReversalStrategy {
    config: ReversalConfig,

    // Per-symbol CEX state
    spot: HashMap<String, SpotState>,
    return_buffers: HashMap<String, ReturnBuffer>,  // reuse from directional.rs
    lob_depth: HashMap<String, LobDepthState>,      // new: near-spot bid/ask depth

    // Per-symbol PM state
    quotes: HashMap<String, QuoteState>,            // token_id → ask
    events: HashMap<String, Vec<EventWindow>>,      // symbol → active windows

    // Per-symbol reversal tracking
    drift_history: HashMap<String, DriftHistory>,   // tracks flip events

    // Risk
    last_entry: HashMap<String, DateTime<Utc>>,     // cooldown
    daily_trade_count: u32,
    daily_reset_date: Option<chrono::NaiveDate>,
}
```

### 3.2 New State Types

```rust
struct LobDepthState {
    bid_depth_near: f64,   // total bid volume within config.lob_depth_pct of spot
    ask_depth_near: f64,
    ts: DateTime<Utc>,
}

struct DriftHistory {
    current_direction: f64,    // +1.0 or -1.0
    flip_ts: Option<DateTime<Utc>>,  // when the last flip happened
    pre_flip_drift: f64,       // drift speed before the flip
    post_flip_drift: f64,      // drift speed after the flip
}
```

### 3.3 Gate Pipeline (5 gates)

```
Gate 0: Price validity — spot > 0, price_to_beat present
Gate 1: Position filter — |distance_to_beat_pct| < config.max_distance_pct
Gate 2: Reversal signal — drift flipped within last N secs, post-flip drift > threshold
Gate 3: LOB confirmation — near-depth favors reversal direction
Gate 4: PM mispricing — token ask < config.max_ask_for_reversal (e.g. 0.25)
         AND time since last PM quote < config.max_pm_lag_secs
Gate 5: Edge — (1.0 - ask - fee) * win_rate_estimate > config.min_edge
         Time filter — min_time_remaining < t < max_time_remaining
```

### 3.4 Config

```toml
[strategy]
symbols = ["BTCUSDT", "DOGEUSDT"]
max_distance_pct = 0.015        # spot within 1.5% of price_to_beat
max_drift_flip_age_secs = 20    # reversal must be fresh
min_post_flip_drift = 0.0001    # drift speed after flip (per sec)
lob_depth_pct = 0.001           # near-depth window: 0.1% of spot
min_lob_depth_ratio = 1.3       # bid/ask depth ratio for confirmation
max_ask_for_reversal = 0.25     # PM token ask must be below this
max_pm_lag_secs = 30            # PM quote must be fresh
min_time_remaining_secs = 60
max_time_remaining_secs = 240
cooldown_secs = 90
stake_usd = 10.0
min_edge = 0.05
allowed_window_secs = [300]
```

### 3.5 Exit Logic

Reversal trades have a natural exit: settlement. But early exits are needed:

- **Take profit:** PM ask rises above `config.take_profit_ask` (e.g. 0.65) — sell back
- **Stop loss:** spot moves further away from `price_to_beat` beyond `config.stop_distance_pct`
- **Time stop:** if `time_remaining < 30s` and position not profitable, exit at market

### 3.6 Config Files

```
config/strategies/05-reversal.dryrun.toml
config/strategies/05-reversal.live.toml
config/strategies/05-reversal.backtest.toml
```

---

## 4. Data Requirements

| Data | Table | Status | Gap |
|------|-------|--------|-----|
| CEX spot ticks | `sync_records` / `binance_price_ticks` | ✅ loaded | — |
| CEX agg trades | `binance_aggtrade_ticks` | ✅ loaded | — |
| CEX LOB (OBI, spread) | `binance_lob_ticks` | ✅ loaded (obi_5 only) | Need near-depth from `bids`/`asks` JSONB |
| PM quotes | `clob_quote_ticks` | ✅ loaded | Need quote freshness timestamp |
| PM events | `pm_market_metadata` | ✅ loaded | — |
| PM settlements | `pm_token_settlements` | ✅ loaded | — |

**One new loader change:** `load_l2_data` needs to also fetch `bids` and `asks` JSONB
columns and parse near-depth. This is additive — existing `MarketUpdate::L2` is unchanged,
a new `MarketUpdate::L2Depth` variant carries the parsed values.

---

## 5. Testing Approach

### Phase 1 Tests
- Unit test for `near_depth()` parser with known JSONB input
- Integration test: attribution tool runs on synthetic data and produces expected CSV columns

### Phase 2 Tests
- Unit tests for each gate in isolation (same pattern as `directional.rs`)
- `reversal_signal_triggers_entry_on_drift_flip()` — core happy path
- `no_entry_when_spot_too_far_from_price_to_beat()` — Gate 1 rejection
- `no_entry_when_lob_depth_opposes_direction()` — Gate 3 rejection
- `no_entry_when_pm_ask_already_high()` — Gate 4 rejection
- Backtest integration test on synthetic reversal scenario

---

## 6. Implementation Order

1. **`MarketUpdate::L2Depth` variant** — additive, no breaking changes
2. **`load_l2_data` extension** — parse `bids`/`asks` JSONB into near-depth
3. **`signal_attribution` example** — Phase 1 research tool
4. **Run attribution on real data** — validate alpha before writing strategy
5. **`reversal.rs`** — `ReversalStrategy` + `ReversalConfig`
6. **Config files** — dryrun + backtest TOMLs
7. **Backtest validation** — run on same clean windows used for DirectionalStrategy comparison

---

## 7. What This Does NOT Change

- `DirectionalStrategy` — untouched, continues running on tango-1-1
- `StrategyRuntime` — no changes needed, `ReversalStrategy` plugs in via `StrategyLogic`
- Existing config files — no changes to `02-pm5d.*` configs
- Settlement pipeline — unchanged
- Deployment workflow — new strategy deploys via same `ployd` managed deployment path
