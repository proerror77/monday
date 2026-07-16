# PM5D Reversal Strategy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a signal attribution research tool and a `ReversalStrategy` that enters when CEX momentum reverses near `price_to_beat` while PM quotes lag behind.

**Architecture:** Phase 1 adds a `signal_attribution` example that exports per-tick feature rows to CSV for offline analysis. Phase 2 adds `ReversalStrategy` as an independent `StrategyLogic` impl that plugs into the existing `StrategyRuntime` without touching `DirectionalStrategy`.

**Tech Stack:** Rust, sqlx/PostgreSQL, `ploy-strategy-bundles`, `chrono`, `rust_decimal`, `serde_json` (for JSONB depth parsing), `csv` crate for output.

---

## File Map

| Action | Path | Responsibility |
|--------|------|----------------|
| Modify | `crates/ploy-strategy-bundles/src/traits.rs` | Add `MarketUpdate::L2Depth` variant |
| Modify | `crates/ploy-strategy-bundles/src/feed/database.rs` | Extend `load_l2_data` to parse `bids`/`asks` JSONB |
| Modify | `crates/ploy-strategy-bundles/src/feed/mod.rs` | Re-export new loader option |
| Create | `crates/ploy-strategy-bundles/examples/signal_attribution.rs` | Phase 1 research tool |
| Create | `crates/ploy-strategy-bundles/src/strategies/reversal.rs` | `ReversalStrategy` + `ReversalConfig` |
| Modify | `crates/ploy-strategy-bundles/src/strategies/mod.rs` | Export `ReversalStrategy` |
| Modify | `crates/ploy-strategy-bundles/src/lib.rs` | Re-export `ReversalStrategy` |
| Create | `config/strategies/05-reversal.dryrun.toml` | Dry-run config |
| Create | `config/strategies/05-reversal.backtest.toml` | Backtest config |

---

## Task 1: Add `MarketUpdate::L2Depth` variant

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/traits.rs`
- Modify: `crates/ploy-strategy-bundles/src/feed/database.rs` (update `update_ts` match)

- [ ] **Step 1: Write failing test**

In `crates/ploy-strategy-bundles/src/feed/database.rs`, add at the bottom of the test module:

```rust
#[test]
fn l2depth_variant_round_trips_through_update_ts() {
    use crate::traits::MarketUpdate;
    use chrono::Utc;
    let ts = Utc::now();
    let u = MarketUpdate::L2Depth {
        symbol: "BTCUSDT".to_string(),
        obi: 0.3,
        spread_bps: 5,
        bid_depth_near: 12.5,
        ask_depth_near: 8.0,
        ts,
    };
    let got = update_ts(&u);
    assert_eq!(got, ts);
}
```

- [ ] **Step 2: Run test to confirm it fails**

```bash
cargo test -p ploy-strategy-bundles l2depth_variant -- --nocapture 2>&1 | tail -5
```
Expected: compile error — `L2Depth` variant does not exist yet.

- [ ] **Step 3: Add variant to `MarketUpdate` in `traits.rs`**

After the existing `L2` variant (around line 55), add:

```rust
/// CEX L2 orderbook with parsed near-spot depth from JSONB.
L2Depth {
    symbol: String,
    obi: f64,
    spread_bps: u32,
    bid_depth_near: f64,
    ask_depth_near: f64,
    ts: DateTime<Utc>,
},
```

- [ ] **Step 4: Update `update_ts` in `database.rs`**

In `update_ts` (around line 201), add `L2Depth` to the first match arm:

```rust
MarketUpdate::SpotPrice { ts, .. }
| MarketUpdate::AggTrade { ts, .. }
| MarketUpdate::Quote { ts, .. }
| MarketUpdate::L2 { ts, .. }
| MarketUpdate::L2Depth { ts, .. }
| MarketUpdate::SportsState { ts, .. }
| MarketUpdate::ReferencePrice { ts, .. }
| MarketUpdate::Kline { ts, .. } => *ts,
```

- [ ] **Step 5: Run test to confirm it passes**

```bash
cargo test -p ploy-strategy-bundles l2depth_variant -- --nocapture 2>&1 | tail -5
```
Expected: `test l2depth_variant_round_trips_through_update_ts ... ok`

- [ ] **Step 6: Check compile**

```bash
cargo check -p ploy-strategy-bundles --all-targets 2>&1 | tail -10
```

- [ ] **Step 7: Commit**

```bash
git add crates/ploy-strategy-bundles/src/traits.rs \
        crates/ploy-strategy-bundles/src/feed/database.rs
git commit -m "feat: add MarketUpdate::L2Depth variant for near-spot LOB depth"
```

---

## Task 2: Extend `load_l2_data` to emit `L2Depth` updates

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/feed/database.rs`

- [ ] **Step 1: Write failing test**

Add to the test module in `database.rs`:

```rust
#[test]
fn near_depth_sums_volume_within_pct_range() {
    // bids: [[price, qty], ...]
    let bids = serde_json::json!([
        ["100.0", "5.0"],
        ["99.5", "3.0"],   // within 0.1% of 100.0 (threshold = 99.9)
        ["98.0", "10.0"],  // outside range
    ]);
    let asks = serde_json::json!([
        ["100.1", "4.0"],  // within 0.1% of 100.0 (threshold = 100.1)
        ["101.0", "7.0"],  // outside range
    ]);
    let (bid_near, ask_near) = near_depth(&bids, &asks, 100.0, 0.001);
    // bid: 99.5 is within 0.1% (100.0 * 0.999 = 99.9), 98.0 is not
    assert!((bid_near - 3.0).abs() < 0.001, "bid_near={bid_near}");
    // ask: 100.1 is within 0.1% (100.0 * 1.001 = 100.1), 101.0 is not
    assert!((ask_near - 4.0).abs() < 0.001, "ask_near={ask_near}");
}
```

- [ ] **Step 2: Run test to confirm it fails**

```bash
cargo test -p ploy-strategy-bundles near_depth_sums -- --nocapture 2>&1 | tail -5
```
Expected: compile error — `near_depth` not defined.

- [ ] **Step 3: Add `near_depth` helper to `database.rs`**

Add before `load_l2_data`:

```rust
/// Parse near-spot bid/ask depth from JSONB level arrays.
/// Each level is `[price_str, qty_str]`. Returns (bid_volume_near, ask_volume_near)
/// where "near" means within `pct_range` of `spot` (e.g. 0.001 = 0.1%).
fn near_depth(
    bids: &serde_json::Value,
    asks: &serde_json::Value,
    spot: f64,
    pct_range: f64,
) -> (f64, f64) {
    let bid_min = spot * (1.0 - pct_range);
    let ask_max = spot * (1.0 + pct_range);

    let sum_near = |levels: &serde_json::Value, min: f64, max: f64| -> f64 {
        levels
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|level| {
                        let price: f64 = level.get(0)?.as_str()?.parse().ok()?;
                        let qty: f64 = level.get(1)?.as_str()?.parse().ok()?;
                        if price >= min && price <= max {
                            Some(qty)
                        } else {
                            None
                        }
                    })
                    .sum()
            })
            .unwrap_or(0.0)
    };

    (sum_near(bids, bid_min, spot), sum_near(asks, spot, ask_max))
}
```

- [ ] **Step 4: Extend `load_l2_data` to also fetch JSONB and emit `L2Depth`**

Replace the existing `load_l2_data` function body with:

```rust
async fn load_l2_data(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), sqlx::Error> {
    let rows: Vec<(DateTime<Utc>, String, f64, i32, Option<serde_json::Value>, Option<serde_json::Value>)> =
        sqlx::query_as(
            r#"
            SELECT event_time, symbol,
                   COALESCE(obi_5, 0.0) as obi,
                   COALESCE(spread_bps, 0) as spread_bps,
                   bids,
                   asks
            FROM binance_lob_ticks
            WHERE symbol = ANY($1)
              AND event_time >= $2
              AND event_time <= $3
            ORDER BY event_time
            "#,
        )
        .bind(symbols)
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    info!(count = rows.len(), "Loaded L2 data from binance_lob_ticks");
    for (ts, symbol, obi, spread_bps, bids, asks) in rows {
        // Always emit the existing L2 variant (obi + spread only)
        updates.push(MarketUpdate::L2 {
            symbol: symbol.clone(),
            obi,
            spread_bps: spread_bps as u32,
            ts,
        });
        // Also emit L2Depth when JSONB depth data is available
        if let (Some(bids_json), Some(asks_json)) = (bids, asks) {
            // Use mid_price from obi as a proxy for spot; actual spot comes from SpotPrice ticks.
            // We store 0.0 here and let the strategy compute near-depth against live spot.
            // For the attribution tool we need the raw depth, so we parse it here.
            let (bid_near, ask_near) = near_depth(&bids_json, &asks_json, 0.0, 0.001);
            // Only emit if we got non-trivial data
            if bid_near > 0.0 || ask_near > 0.0 {
                updates.push(MarketUpdate::L2Depth {
                    symbol,
                    obi,
                    spread_bps: spread_bps as u32,
                    bid_depth_near: bid_near,
                    ask_depth_near: ask_near,
                    ts,
                });
            }
        }
    }
    Ok(())
}
```

**Note:** The `near_depth` call above uses `spot=0.0` as a placeholder — the attribution tool will re-compute near-depth against the actual spot price at each tick. The `L2Depth` variant here carries the raw parsed totals; the strategy will use live spot to filter.

- [ ] **Step 5: Run tests**

```bash
cargo test -p ploy-strategy-bundles near_depth_sums -- --nocapture 2>&1 | tail -5
cargo check -p ploy-strategy-bundles --all-targets 2>&1 | tail -10
```
Expected: test passes, no compile errors.

- [ ] **Step 6: Commit**

```bash
git add crates/ploy-strategy-bundles/src/feed/database.rs
git commit -m "feat: extend load_l2_data to parse JSONB depth and emit L2Depth updates"
```

---

## Task 3: `signal_attribution` example (Phase 1 research tool)

**Files:**
- Create: `crates/ploy-strategy-bundles/examples/signal_attribution.rs`

This tool replays historical data and writes one CSV row per spot tick per event window, capturing all features needed to validate the reversal alpha hypothesis.

- [ ] **Step 1: Create the file**

```rust
//! Signal attribution tool for PM5D reversal research.
//!
//! Replays historical data and writes one row per (event, spot_tick) pair.
//! Output CSV/Parquet can be analyzed through the repository's Rust/Polars
//! research export path to validate reversal signal quality.
//!
//! Usage:
//!   cargo run --release -p ploy-strategy-bundles --example signal_attribution -- \
//!     --db-url postgresql://user:pass@host/ploy \
//!     --start-date 2026-04-01 \
//!     --end-date 2026-04-10 \
//!     --symbols BTCUSDT,DOGEUSDT \
//!     --output /tmp/attribution.csv

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_strategy_bundles::feed::{load_from_database_with_options, HistoricalLoadOptions};
use ploy_strategy_bundles::traits::MarketUpdate;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use sqlx::postgres::PgPoolOptions;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Write;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn parse_date_start(s: &str) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .unwrap_or_else(|_| panic!("Invalid date: {s}"))
            .and_hms_opt(0, 0, 0)
            .unwrap(),
    )
}

fn parse_date_end(s: &str) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .unwrap_or_else(|_| panic!("Invalid date: {s}"))
            .and_hms_opt(23, 59, 59)
            .unwrap(),
    )
}

/// Ring buffer for computing drift speed over a rolling window.
struct DriftBuffer {
    entries: VecDeque<(DateTime<Utc>, f64)>, // (ts, log_price)
    window_secs: f64,
}

impl DriftBuffer {
    fn new(window_secs: f64) -> Self {
        Self { entries: VecDeque::new(), window_secs }
    }

    fn push(&mut self, ts: DateTime<Utc>, price: f64) {
        self.entries.push_back((ts, price.ln()));
        // Evict entries older than window
        while self.entries.len() > 1 {
            let oldest = self.entries.front().unwrap().0;
            let elapsed = (ts - oldest).num_milliseconds() as f64 / 1000.0;
            if elapsed > self.window_secs {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    /// Returns log-return per second over the window. Positive = upward drift.
    fn drift_speed(&self) -> f64 {
        if self.entries.len() < 2 {
            return 0.0;
        }
        let (t0, p0) = self.entries.front().unwrap();
        let (t1, p1) = self.entries.back().unwrap();
        let dt = ((*t1 - *t0).num_milliseconds() as f64 / 1000.0).max(0.001);
        (p1 - p0) / dt
    }
}

#[derive(Default)]
struct EventState {
    event_id: String,
    symbol: String,
    end_time: Option<DateTime<Utc>>,
    price_to_beat: Option<f64>,
    resolved_up_won: Option<bool>,
    up_token: String,
    down_token: String,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db_url = flag_value(&args, "--db-url").expect("--db-url required");
    let start = parse_date_start(&flag_value(&args, "--start-date").expect("--start-date required"));
    let end = parse_date_end(&flag_value(&args, "--end-date").expect("--end-date required"));
    let symbols_str = flag_value(&args, "--symbols").unwrap_or_else(|| "BTCUSDT,DOGEUSDT".to_string());
    let output_path = flag_value(&args, "--output").unwrap_or_else(|| "/tmp/attribution.csv".to_string());
    let symbols: Vec<String> = symbols_str.split(',').map(|s| s.trim().to_string()).collect();

    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&db_url)
        .await
        .expect("DB connect failed");

    let options = HistoricalLoadOptions {
        require_official_settlement: true,
        ..Default::default()
    };

    eprintln!("Loading data for {:?} from {} to {}", symbols, start, end);
    let updates = load_from_database_with_options(&pool, &symbols, start, end, &options)
        .await
        .expect("Load failed");
    eprintln!("Loaded {} updates", updates.len());

    // State
    let mut spot: HashMap<String, (DateTime<Utc>, f64)> = HashMap::new();
    let mut buf_30s: HashMap<String, DriftBuffer> = HashMap::new();
    let mut buf_10s: HashMap<String, DriftBuffer> = HashMap::new();
    let mut prev_drift_30s: HashMap<String, f64> = HashMap::new();
    let mut drift_flip_ts: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();
    let mut events: HashMap<String, EventState> = HashMap::new(); // event_id → state
    let mut token_to_event: HashMap<String, String> = HashMap::new(); // token_id → event_id
    let mut quotes: HashMap<String, (DateTime<Utc>, f64)> = HashMap::new(); // token_id → (ts, ask)
    let mut lob: HashMap<String, (f64, f64, u32)> = HashMap::new(); // symbol → (bid_near, ask_near, spread_bps)

    let mut out = File::create(&output_path).expect("Cannot create output file");
    writeln!(out, "event_id,symbol,price_to_beat,tick_ts,time_remaining_secs,spot_price,distance_to_beat_pct,drift_speed_30s,drift_speed_10s,drift_direction_flipped,drift_flip_age_secs,obi,spread_bps,lob_bid_depth_near,lob_ask_depth_near,pm_up_ask,pm_down_ask,pm_ask_lag_secs,outcome_up_won").unwrap();

    let mut rows_written = 0usize;

    for update in &updates {
        match update {
            MarketUpdate::EventDiscovered { event_id, symbol, up_token, down_token, end_time, price_to_beat, resolved_up_won, .. } => {
                let ptb = price_to_beat.and_then(|p| p.to_f64());
                token_to_event.insert(up_token.clone(), event_id.clone());
                token_to_event.insert(down_token.clone(), event_id.clone());
                events.insert(event_id.clone(), EventState {
                    event_id: event_id.clone(),
                    symbol: symbol.clone(),
                    end_time: Some(*end_time),
                    price_to_beat: ptb,
                    resolved_up_won: *resolved_up_won,
                    up_token: up_token.clone(),
                    down_token: down_token.clone(),
                });
            }
            MarketUpdate::EventExpired { event_id, resolved_up_won, .. } => {
                if let Some(ev) = events.get_mut(event_id) {
                    if resolved_up_won.is_some() {
                        ev.resolved_up_won = *resolved_up_won;
                    }
                }
            }
            MarketUpdate::Quote { token_id, ask, ts, .. } => {
                if let Some(ask_price) = ask.and_then(|a| a.to_f64()) {
                    quotes.insert(token_id.clone(), (*ts, ask_price));
                }
            }
            MarketUpdate::L2Depth { symbol, bid_depth_near, ask_depth_near, spread_bps, obi, .. } => {
                lob.insert(symbol.clone(), (*bid_depth_near, *ask_depth_near, *spread_bps));
                let _ = obi; // obi available via L2 variant
            }
            MarketUpdate::SpotPrice { symbol, price, ts } => {
                let price_f = match price.to_f64() { Some(p) => p, None => continue };

                // Update drift buffers
                buf_30s.entry(symbol.clone()).or_insert_with(|| DriftBuffer::new(30.0)).push(*ts, price_f);
                buf_10s.entry(symbol.clone()).or_insert_with(|| DriftBuffer::new(10.0)).push(*ts, price_f);

                let drift_30 = buf_30s[symbol].drift_speed();
                let drift_10 = buf_10s[symbol].drift_speed();

                // Detect drift direction flip
                let prev = prev_drift_30s.get(symbol).copied().unwrap_or(0.0);
                let flipped = prev != 0.0 && drift_30 != 0.0 && prev.signum() != drift_30.signum();
                if flipped {
                    drift_flip_ts.insert(symbol.clone(), Some(*ts));
                }
                prev_drift_30s.insert(symbol.clone(), drift_30);
                spot.insert(symbol.clone(), (*ts, price_f));

                // Emit one row per active event for this symbol
                for ev in events.values() {
                    if ev.symbol != *symbol { continue; }
                    let ptb = match ev.price_to_beat { Some(p) => p, None => continue };
                    let end_time = match ev.end_time { Some(t) => t, None => continue };
                    let time_remaining = (end_time - *ts).num_seconds();
                    if time_remaining < 0 { continue; }

                    let distance_pct = (price_f - ptb) / ptb;
                    let flip_age = drift_flip_ts.get(symbol)
                        .and_then(|t| *t)
                        .map(|t| (*ts - t).num_seconds() as f64)
                        .unwrap_or(-1.0);
                    let direction_flipped = flip_age >= 0.0 && flip_age < 60.0;

                    let (up_ask, up_lag) = quotes.get(&ev.up_token)
                        .map(|(qt, ask)| (*ask, (*ts - *qt).num_seconds() as f64))
                        .unwrap_or((-1.0, -1.0));
                    let (dn_ask, _) = quotes.get(&ev.down_token)
                        .map(|(qt, ask)| (*ask, (*ts - *qt).num_seconds() as f64))
                        .unwrap_or((-1.0, -1.0));

                    let (bid_near, ask_near, spread_bps) = lob.get(symbol).copied().unwrap_or((0.0, 0.0, 0));
                    let outcome = ev.resolved_up_won.map(|b| if b { "1" } else { "0" }).unwrap_or("");

                    writeln!(out,
                        "{},{},{:.6},{},{},{:.6},{:.6},{:.8},{:.8},{},{:.1},{:.1},{},{:.4},{:.4},{:.4},{:.4},{:.1},{}",
                        ev.event_id, symbol, ptb,
                        ts.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                        time_remaining, price_f, distance_pct,
                        drift_30, drift_10,
                        if direction_flipped { 1 } else { 0 },
                        flip_age,
                        0.0_f64, // obi placeholder — comes from L2 variant
                        spread_bps,
                        bid_near, ask_near,
                        up_ask, dn_ask, up_lag,
                        outcome
                    ).unwrap();
                    rows_written += 1;
                }
            }
            _ => {}
        }
    }

    eprintln!("Wrote {} rows to {}", rows_written, output_path);
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo build --release -p ploy-strategy-bundles --example signal_attribution 2>&1 | tail -15
```
Expected: compiles cleanly (no DB connection needed to build).

- [ ] **Step 3: Commit**

```bash
git add crates/ploy-strategy-bundles/examples/signal_attribution.rs
git commit -m "feat: add signal_attribution example for reversal alpha research"
```

---

## Task 4: Run attribution on real data (research gate)

This task is manual — run the tool, inspect the output, then decide Phase 2 gate thresholds.

- [ ] **Step 1: Run attribution on a clean window**

```bash
cargo run --release -p ploy-strategy-bundles --example signal_attribution -- \
  --db-url "$PLOY_DB_URL" \
  --start-date 2026-04-08 \
  --end-date 2026-04-11 \
  --symbols BTCUSDT,DOGEUSDT \
  --output /tmp/attribution.csv
```

- [ ] **Step 2: Analyze with Rust/Polars**

```bash
cargo run --release -p ploy-research --example event_factor_attribution \
  --features polars-export -- \
  --dataset /path/to/event-root-dataset \
  --output-dir /tmp/attribution
```

Keep the analysis in the Rust/Polars path so the research result uses the
same typed schema and Parquet/CSV semantics as backtests and promotion gates.

- [ ] **Step 3: Record findings in `tasks/todo.md`**

Add a section with the empirical thresholds found:
- Best `max_distance_pct`
- Best `max_drift_flip_age_secs`
- Whether LOB depth ratio improves win rate
- Typical PM lag window

These values go directly into Task 5's `ReversalConfig`.

---

## Task 5: `ReversalStrategy` — core struct and state

**Files:**
- Create: `crates/ploy-strategy-bundles/src/strategies/reversal.rs`

- [ ] **Step 1: Write failing test**

Create `crates/ploy-strategy-bundles/src/strategies/reversal.rs` with just the test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversal_strategy_has_correct_name() {
        let config = ReversalConfig::default();
        let strat = ReversalStrategy::new(config);
        assert_eq!(strat.name(), "pm5d_reversal");
    }
}
```

- [ ] **Step 2: Run test to confirm it fails**

```bash
cargo test -p ploy-strategy-bundles reversal_strategy_has_correct_name 2>&1 | tail -5
```
Expected: compile error — `ReversalConfig`, `ReversalStrategy` not defined.

- [ ] **Step 3: Add `ReversalConfig` and skeleton `ReversalStrategy`**

```rust
use std::collections::HashMap;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use ploy_trading::{FillRecord, OrderLedger, PositionLedger};
use crate::traits::{MarketUpdate, StrategyDecision, StrategyLogic};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReversalConfig {
    #[serde(default = "default_symbols")]
    pub symbols: Vec<String>,
    /// Max |distance| from price_to_beat as fraction (e.g. 0.015 = 1.5%)
    #[serde(default = "default_max_distance_pct")]
    pub max_distance_pct: f64,
    /// Reversal must have happened within this many seconds
    #[serde(default = "default_max_drift_flip_age_secs")]
    pub max_drift_flip_age_secs: u64,
    /// Minimum drift speed after flip (log-return/sec)
    #[serde(default = "default_min_post_flip_drift")]
    pub min_post_flip_drift: f64,
    /// Near-depth window as fraction of spot (e.g. 0.001 = 0.1%)
    #[serde(default = "default_lob_depth_pct")]
    pub lob_depth_pct: f64,
    /// Minimum bid/ask depth ratio for LOB confirmation
    #[serde(default = "default_min_lob_depth_ratio")]
    pub min_lob_depth_ratio: f64,
    /// PM token ask must be below this to qualify as mispriced
    #[serde(default = "default_max_ask_for_reversal")]
    pub max_ask_for_reversal: f64,
    /// PM quote must be fresher than this many seconds
    #[serde(default = "default_max_pm_lag_secs")]
    pub max_pm_lag_secs: u64,
    #[serde(default = "default_min_time")]
    pub min_time_remaining_secs: u64,
    #[serde(default = "default_max_time")]
    pub max_time_remaining_secs: u64,
    #[serde(default = "default_cooldown")]
    pub cooldown_secs: u64,
    #[serde(default = "default_stake_usd")]
    pub stake_usd: Decimal,
    #[serde(default = "default_min_edge")]
    pub min_edge: f64,
    #[serde(default)]
    pub allowed_window_secs: Vec<u64>,
    #[serde(default)]
    pub max_daily_loss_usd: Option<Decimal>,
}

fn default_symbols() -> Vec<String> { vec![] }
fn default_max_distance_pct() -> f64 { 0.015 }
fn default_max_drift_flip_age_secs() -> u64 { 20 }
fn default_min_post_flip_drift() -> f64 { 0.0001 }
fn default_lob_depth_pct() -> f64 { 0.001 }
fn default_min_lob_depth_ratio() -> f64 { 1.3 }
fn default_max_ask_for_reversal() -> f64 { 0.25 }
fn default_max_pm_lag_secs() -> u64 { 30 }
fn default_min_time() -> u64 { 60 }
fn default_max_time() -> u64 { 240 }
fn default_cooldown() -> u64 { 90 }
fn default_stake_usd() -> Decimal { rust_decimal_macros::dec!(10.0) }
fn default_min_edge() -> f64 { 0.05 }

impl Default for ReversalConfig {
    fn default() -> Self {
        Self {
            symbols: default_symbols(),
            max_distance_pct: default_max_distance_pct(),
            max_drift_flip_age_secs: default_max_drift_flip_age_secs(),
            min_post_flip_drift: default_min_post_flip_drift(),
            lob_depth_pct: default_lob_depth_pct(),
            min_lob_depth_ratio: default_min_lob_depth_ratio(),
            max_ask_for_reversal: default_max_ask_for_reversal(),
            max_pm_lag_secs: default_max_pm_lag_secs(),
            min_time_remaining_secs: default_min_time(),
            max_time_remaining_secs: default_max_time(),
            cooldown_secs: default_cooldown(),
            stake_usd: default_stake_usd(),
            min_edge: default_min_edge(),
            allowed_window_secs: vec![300],
            max_daily_loss_usd: None,
        }
    }
}

// ── Internal state types ──────────────────────────────────

struct SpotState { price: rust_decimal::Decimal, ts: DateTime<Utc> }
struct QuoteState { ask: Option<rust_decimal::Decimal>, ts: DateTime<Utc> }

#[derive(Default)]
struct EventWindow {
    event_id: String,
    symbol: String,
    up_token: String,
    down_token: String,
    end_time: DateTime<Utc>,
    window_secs: u64,
    price_to_beat: Option<rust_decimal::Decimal>,
}

#[derive(Default)]
struct DriftHistory {
    /// +1.0 = upward, -1.0 = downward, 0.0 = unknown
    current_direction: f64,
    flip_ts: Option<DateTime<Utc>>,
    post_flip_drift: f64,
}

#[derive(Default)]
struct LobDepthState {
    bid_depth_near: f64,
    ask_depth_near: f64,
    ts: Option<DateTime<Utc>>,
}

// ── Strategy ─────────────────────────────────────────────

pub struct ReversalStrategy {
    config: ReversalConfig,
    spot: HashMap<String, SpotState>,
    quotes: HashMap<String, QuoteState>,
    events: HashMap<String, Vec<EventWindow>>,
    token_symbol: HashMap<String, String>,
    drift: HashMap<String, DriftHistory>,
    lob: HashMap<String, LobDepthState>,
    // Simple return buffer for drift computation (30s window)
    price_history: HashMap<String, std::collections::VecDeque<(DateTime<Utc>, f64)>>,
    last_entry: HashMap<String, DateTime<Utc>>,
    daily_trade_count: u32,
    daily_reset_date: Option<NaiveDate>,
}

impl ReversalStrategy {
    pub fn new(config: ReversalConfig) -> Self {
        Self {
            config,
            spot: HashMap::new(),
            quotes: HashMap::new(),
            events: HashMap::new(),
            token_symbol: HashMap::new(),
            drift: HashMap::new(),
            lob: HashMap::new(),
            price_history: HashMap::new(),
            last_entry: HashMap::new(),
            daily_trade_count: 0,
            daily_reset_date: None,
        }
    }
}

impl StrategyLogic for ReversalStrategy {
    fn on_update(
        &mut self,
        _update: &MarketUpdate,
        _positions: &PositionLedger,
        _orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        vec![] // implemented in Task 6
    }

    fn on_fill(&mut self, _fill: &FillRecord) {}

    fn name(&self) -> &str {
        "pm5d_reversal"
    }
}
```

- [ ] **Step 4: Run test to confirm it passes**

```bash
cargo test -p ploy-strategy-bundles reversal_strategy_has_correct_name -- --nocapture 2>&1 | tail -5
```
Expected: `test reversal_strategy_has_correct_name ... ok`

- [ ] **Step 5: Wire into module exports**

In `crates/ploy-strategy-bundles/src/strategies/mod.rs`, add:
```rust
pub mod reversal;
pub use reversal::ReversalStrategy;
```

In `crates/ploy-strategy-bundles/src/lib.rs`, add to the strategies re-export line:
```rust
pub use strategies::ReversalStrategy;
```

- [ ] **Step 6: Check compile**

```bash
cargo check -p ploy-strategy-bundles --all-targets 2>&1 | tail -10
```

- [ ] **Step 7: Commit**

```bash
git add crates/ploy-strategy-bundles/src/strategies/reversal.rs \
        crates/ploy-strategy-bundles/src/strategies/mod.rs \
        crates/ploy-strategy-bundles/src/lib.rs
git commit -m "feat: add ReversalStrategy skeleton with config and state types"
```

---

## Task 6: Implement `on_update` gate pipeline

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/strategies/reversal.rs`

- [ ] **Step 1: Write failing test**

Add to the `tests` module in `reversal.rs`:

```rust
#[test]
fn reversal_signal_triggers_entry_on_drift_flip() {
    use chrono::Duration;
    use ploy_trading::{OrderLedger, PositionLedger};
    use rust_decimal_macros::dec;

    let config = ReversalConfig {
        symbols: vec!["BTCUSDT".to_string()],
        max_distance_pct: 0.02,
        max_drift_flip_age_secs: 30,
        min_post_flip_drift: 0.00001,
        min_lob_depth_ratio: 1.0,
        max_ask_for_reversal: 0.40,
        max_pm_lag_secs: 60,
        min_time_remaining_secs: 30,
        max_time_remaining_secs: 300,
        cooldown_secs: 0,
        min_edge: -1.0, // disable edge gate for test
        ..ReversalConfig::default()
    };
    let mut strat = ReversalStrategy::new(config);
    let positions = PositionLedger::default();
    let orders = OrderLedger::default();
    let now = Utc::now();
    let end_time = now + Duration::seconds(180);

    strat.on_update(&MarketUpdate::EventDiscovered {
        event_id: "evt1".to_string(),
        symbol: "BTCUSDT".to_string(),
        up_token: "up1".to_string(),
        down_token: "dn1".to_string(),
        end_time,
        window_secs: 300,
        price_to_beat: Some(dec!(100.0)),
        resolved_up_won: None,
    }, &positions, &orders);

    // Downward drift (prices falling)
    for i in 0..5i64 {
        strat.on_update(&MarketUpdate::SpotPrice {
            symbol: "BTCUSDT".to_string(),
            price: dec!(99.8) - rust_decimal::Decimal::from(i) * dec!(0.05),
            ts: now - Duration::seconds(40 - i * 7),
        }, &positions, &orders);
    }

    // Upward drift (prices rising — triggers flip)
    for i in 0..3i64 {
        strat.on_update(&MarketUpdate::SpotPrice {
            symbol: "BTCUSDT".to_string(),
            price: dec!(99.6) + rust_decimal::Decimal::from(i) * dec!(0.05),
            ts: now - Duration::seconds(15 - i * 5),
        }, &positions, &orders);
    }

    // LOB depth favoring UP
    strat.on_update(&MarketUpdate::L2Depth {
        symbol: "BTCUSDT".to_string(),
        obi: 0.3,
        spread_bps: 5,
        bid_depth_near: 10.0,
        ask_depth_near: 5.0,
        ts: now - Duration::seconds(2),
    }, &positions, &orders);

    // PM quote with low ask
    strat.on_update(&MarketUpdate::Quote {
        token_id: "up1".to_string(),
        bid: Some(dec!(0.18)),
        ask: Some(dec!(0.22)),
        ts: now - Duration::seconds(1),
    }, &positions, &orders);

    // Final spot price — should trigger entry
    let decisions = strat.on_update(&MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".to_string(),
        price: dec!(99.7),
        ts: now,
    }, &positions, &orders);

    assert!(
        decisions.iter().any(|d| matches!(d, StrategyDecision::Enter { .. })),
        "Expected Enter decision, got: {:?}", decisions
    );
}
```

- [ ] **Step 2: Run test to confirm it fails**

```bash
cargo test -p ploy-strategy-bundles reversal_signal_triggers_entry -- --nocapture 2>&1 | tail -5
```
Expected: test fails — `on_update` returns empty vec.

- [ ] **Step 3: Replace stub `on_update` with full implementation**

Replace the `impl StrategyLogic for ReversalStrategy` block:

```rust
impl StrategyLogic for ReversalStrategy {
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        match update {
            MarketUpdate::EventDiscovered {
                event_id, symbol, up_token, down_token,
                end_time, window_secs, price_to_beat, ..
            } => {
                if !self.config.allowed_window_secs.is_empty()
                    && !self.config.allowed_window_secs.contains(window_secs)
                {
                    return vec![];
                }
                let ev = EventWindow {
                    event_id: event_id.clone(),
                    symbol: symbol.clone(),
                    up_token: up_token.clone(),
                    down_token: down_token.clone(),
                    end_time: *end_time,
                    window_secs: *window_secs,
                    price_to_beat: *price_to_beat,
                };
                self.token_symbol.insert(up_token.clone(), symbol.clone());
                self.token_symbol.insert(down_token.clone(), symbol.clone());
                self.events.entry(symbol.clone()).or_default().push(ev);
            }
            MarketUpdate::EventExpired { event_id, .. } => {
                for windows in self.events.values_mut() {
                    windows.retain(|w| &w.event_id != event_id);
                }
            }
            MarketUpdate::Quote { token_id, ask, ts, .. } => {
                self.quotes.insert(token_id.clone(), QuoteState { ask: *ask, ts: *ts });
            }
            MarketUpdate::L2Depth { symbol, bid_depth_near, ask_depth_near, ts, .. } => {
                let entry = self.lob.entry(symbol.clone()).or_default();
                entry.bid_depth_near = *bid_depth_near;
                entry.ask_depth_near = *ask_depth_near;
                entry.ts = Some(*ts);
            }
            MarketUpdate::SpotPrice { symbol, price, ts } => {
                return self.handle_spot(symbol, price, ts, positions, orders);
            }
            _ => {}
        }
        vec![]
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        if let Some(symbol) = self.token_symbol.get(&fill.token_id).cloned() {
            self.last_entry.insert(symbol, fill.filled_at);
            self.daily_trade_count += 1;
        }
    }

    fn name(&self) -> &str { "pm5d_reversal" }
}
```

- [ ] **Step 4: Add `handle_spot` and `evaluate_entry` to `impl ReversalStrategy`**

Add after the `StrategyLogic` impl block:

```rust
impl ReversalStrategy {
    fn handle_spot(
        &mut self,
        symbol: &str,
        price: &Decimal,
        ts: &DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        let price_f = match price.to_f64() { Some(p) => p, None => return vec![] };

        // Update rolling price history (35s window for 30s drift)
        let history = self.price_history.entry(symbol.to_string()).or_default();
        history.push_back((*ts, price_f.ln()));
        while history.len() > 1 {
            let oldest = history.front().unwrap().0;
            if (*ts - oldest).num_seconds() > 35 { history.pop_front(); } else { break; }
        }

        // Compute 30s drift speed (log-return per second)
        let drift_30s = if history.len() >= 2 {
            let (t0, p0) = history.front().unwrap();
            let (t1, p1) = history.back().unwrap();
            let dt = ((*t1 - *t0).num_milliseconds() as f64 / 1000.0).max(0.001);
            (p1 - p0) / dt
        } else { 0.0 };

        // Detect drift direction flip
        let new_dir = if drift_30s > 1e-7 { 1.0 } else if drift_30s < -1e-7 { -1.0 } else { 0.0 };
        {
            let ds = self.drift.entry(symbol.to_string()).or_default();
            if new_dir != 0.0 && ds.current_direction != 0.0 && new_dir != ds.current_direction {
                ds.flip_ts = Some(*ts);
            }
            if new_dir != 0.0 {
                ds.current_direction = new_dir;
                if ds.flip_ts.is_some() { ds.post_flip_drift = drift_30s.abs(); }
            }
        }

        self.spot.insert(symbol.to_string(), SpotState { price: *price, ts: *ts });

        let today = ts.date_naive();
        if self.daily_reset_date != Some(today) {
            self.daily_trade_count = 0;
            self.daily_reset_date = Some(today);
        }

        let events = match self.events.get(symbol) { Some(e) => e.clone(), None => return vec![] };
        let mut decisions = vec![];
        for ev in &events {
            if let Some(d) = self.evaluate_entry(ev, symbol, price_f, *ts, positions, orders) {
                decisions.push(d);
            }
        }
        decisions
    }

    fn evaluate_entry(
        &self,
        ev: &EventWindow,
        symbol: &str,
        spot: f64,
        now: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Option<StrategyDecision> {
        // Gate 0: price_to_beat must be present and valid
        let ptb = ev.price_to_beat?.to_f64()?;
        if ptb <= 0.0 || spot <= 0.0 { return None; }

        // Gate 1: spot must be within max_distance_pct of price_to_beat
        let distance_pct = (spot - ptb) / ptb;
        if distance_pct.abs() > self.config.max_distance_pct { return None; }

        // Gate 2: drift must have flipped recently with sufficient post-flip speed
        let ds = self.drift.get(symbol)?;
        let flip_ts = ds.flip_ts?;
        let flip_age = (now - flip_ts).num_seconds() as u64;
        if flip_age > self.config.max_drift_flip_age_secs { return None; }
        if ds.post_flip_drift < self.config.min_post_flip_drift { return None; }
        let betting_up = ds.current_direction > 0.0;

        // Gate 3: LOB near-depth must favor reversal direction
        if let Some(lob) = self.lob.get(symbol) {
            let total = lob.bid_depth_near + lob.ask_depth_near;
            if total > 0.0 {
                let ratio = if betting_up {
                    lob.bid_depth_near / lob.ask_depth_near.max(0.001)
                } else {
                    lob.ask_depth_near / lob.bid_depth_near.max(0.001)
                };
                if ratio < self.config.min_lob_depth_ratio { return None; }
            }
        }

        // Gate 4: PM ask must be below threshold and quote must be fresh
        let token_id = if betting_up { &ev.up_token } else { &ev.down_token };
        let quote = self.quotes.get(token_id)?;
        let ask = quote.ask?.to_f64()?;
        if ask >= self.config.max_ask_for_reversal { return None; }
        if (now - quote.ts).num_seconds() as u64 > self.config.max_pm_lag_secs { return None; }

        // Gate 5: time window and edge
        let time_remaining = (ev.end_time - now).num_seconds();
        if time_remaining < self.config.min_time_remaining_secs as i64 { return None; }
        if time_remaining > self.config.max_time_remaining_secs as i64 { return None; }
        let edge = (1.0 - ask - 0.02) * 0.5 - ask * 0.5;
        if edge < self.config.min_edge { return None; }

        // Cooldown and dedup
        if let Some(last) = self.last_entry.get(symbol) {
            if (now - *last).num_seconds() < self.config.cooldown_secs as i64 { return None; }
        }
        if positions.net_qty(token_id) > Decimal::ZERO { return None; }
        if orders.orders().any(|o| {
            o.intent.token_id == *token_id
                && matches!(o.state, ploy_trading::OrderState::Pending | ploy_trading::OrderState::PartiallyFilled)
        }) { return None; }

        let entry_price = Decimal::try_from(ask).ok()?;
        let quantity = if entry_price > Decimal::ZERO {
            (self.config.stake_usd / entry_price).round_dp(6)
        } else { return None; };

        Some(StrategyDecision::Enter {
            intent: TradingIntent {
                intent_id: format!("rev_{}_{}_{}", symbol,
                    if betting_up { "UP" } else { "DN" }, now.timestamp_millis()),
                deployment_id: String::new(),
                market_id: ev.event_id.clone(),
                token_id: token_id.clone(),
                side: TradeSide::Buy,
                quantity,
                limit_price: Some(entry_price),
                purpose: IntentPurpose::Entry,
                created_at: now,
            },
            signal: None,
        })
    }
}
```

- [ ] **Step 5: Add missing imports at top of `reversal.rs`**

Ensure these are present at the top of the file (replace the existing `use` block):

```rust
use std::collections::{HashMap, VecDeque};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use ploy_trading::{FillRecord, IntentPurpose, OrderLedger, PositionLedger, TradeSide, TradingIntent};
use crate::traits::{MarketUpdate, StrategyDecision, StrategyLogic};
```

- [ ] **Step 6: Run tests**

```bash
cargo test -p ploy-strategy-bundles reversal_signal_triggers_entry -- --nocapture 2>&1 | tail -10
cargo check -p ploy-strategy-bundles --all-targets 2>&1 | tail -10
```
Expected: test passes, no compile errors.

- [ ] **Step 7: Commit**

```bash
git add crates/ploy-strategy-bundles/src/strategies/reversal.rs
git commit -m "feat: implement ReversalStrategy on_update with 5-gate entry pipeline"
```

---

## Task 7: Exit logic (take-profit, stop-loss, time-stop)

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/strategies/reversal.rs`

- [ ] **Step 1: Write failing tests**

Add to the `tests` module:

```rust
#[test]
fn no_entry_when_spot_too_far_from_price_to_beat() {
    use chrono::Duration;
    use ploy_trading::{OrderLedger, PositionLedger};
    use rust_decimal_macros::dec;

    let config = ReversalConfig { max_distance_pct: 0.01, ..ReversalConfig::default() };
    let mut strat = ReversalStrategy::new(config);
    let positions = PositionLedger::default();
    let orders = OrderLedger::default();
    let now = Utc::now();

    strat.on_update(&MarketUpdate::EventDiscovered {
        event_id: "evt2".to_string(), symbol: "BTCUSDT".to_string(),
        up_token: "up2".to_string(), down_token: "dn2".to_string(),
        end_time: now + Duration::seconds(180), window_secs: 300,
        price_to_beat: Some(dec!(100.0)), resolved_up_won: None,
    }, &positions, &orders);

    // Spot is 3% away — Gate 1 should reject
    let decisions = strat.on_update(&MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".to_string(), price: dec!(103.0), ts: now,
    }, &positions, &orders);

    assert!(!decisions.iter().any(|d| matches!(d, StrategyDecision::Enter { .. })));
}

#[test]
fn no_entry_when_lob_depth_opposes_direction() {
    use chrono::Duration;
    use ploy_trading::{OrderLedger, PositionLedger};
    use rust_decimal_macros::dec;

    let config = ReversalConfig {
        max_distance_pct: 0.02, max_drift_flip_age_secs: 30,
        min_post_flip_drift: 0.00001, min_lob_depth_ratio: 2.0,
        max_ask_for_reversal: 0.40, max_pm_lag_secs: 60,
        min_time_remaining_secs: 30, max_time_remaining_secs: 300,
        cooldown_secs: 0, min_edge: -1.0,
        ..ReversalConfig::default()
    };
    let mut strat = ReversalStrategy::new(config);
    let positions = PositionLedger::default();
    let orders = OrderLedger::default();
    let now = Utc::now();

    strat.on_update(&MarketUpdate::EventDiscovered {
        event_id: "evt3".to_string(), symbol: "BTCUSDT".to_string(),
        up_token: "up3".to_string(), down_token: "dn3".to_string(),
        end_time: now + Duration::seconds(180), window_secs: 300,
        price_to_beat: Some(dec!(100.0)), resolved_up_won: None,
    }, &positions, &orders);

    // Downward then upward drift (flip to UP)
    for i in 0..4i64 {
        strat.on_update(&MarketUpdate::SpotPrice {
            symbol: "BTCUSDT".to_string(),
            price: dec!(99.8) - rust_decimal::Decimal::from(i) * dec!(0.05),
            ts: now - Duration::seconds(35 - i * 8),
        }, &positions, &orders);
    }
    for i in 0..3i64 {
        strat.on_update(&MarketUpdate::SpotPrice {
            symbol: "BTCUSDT".to_string(),
            price: dec!(99.6) + rust_decimal::Decimal::from(i) * dec!(0.05),
            ts: now - Duration::seconds(12 - i * 4),
        }, &positions, &orders);
    }

    // LOB depth OPPOSING UP (ask > bid) — Gate 3 should reject
    strat.on_update(&MarketUpdate::L2Depth {
        symbol: "BTCUSDT".to_string(), obi: -0.3, spread_bps: 5,
        bid_depth_near: 3.0, ask_depth_near: 10.0,
        ts: now - Duration::seconds(1),
    }, &positions, &orders);

    strat.on_update(&MarketUpdate::Quote {
        token_id: "up3".to_string(), bid: Some(dec!(0.18)),
        ask: Some(dec!(0.22)), ts: now - Duration::seconds(1),
    }, &positions, &orders);

    let decisions = strat.on_update(&MarketUpdate::SpotPrice {
        symbol: "BTCUSDT".to_string(), price: dec!(99.7), ts: now,
    }, &positions, &orders);

    assert!(!decisions.iter().any(|d| matches!(d, StrategyDecision::Enter { .. })),
        "Gate 3 should reject when LOB opposes direction");
}
```

- [ ] **Step 2: Run tests to confirm they pass (gates already implemented)**

```bash
cargo test -p ploy-strategy-bundles "no_entry_when" -- --nocapture 2>&1 | tail -10
```
Expected: both tests pass.

- [ ] **Step 3: Add exit config fields to `ReversalConfig`**

Add two fields to the `ReversalConfig` struct:

```rust
/// PM ask above this triggers take-profit exit (default 0.65)
#[serde(default = "default_take_profit_ask")]
pub take_profit_ask: f64,
/// Spot moving this far from price_to_beat triggers stop-loss (default 0.025)
#[serde(default = "default_stop_distance_pct")]
pub stop_distance_pct: f64,
```

Add default functions:

```rust
fn default_take_profit_ask() -> f64 { 0.65 }
fn default_stop_distance_pct() -> f64 { 0.025 }
```

Update `Default for ReversalConfig` to include:

```rust
take_profit_ask: default_take_profit_ask(),
stop_distance_pct: default_stop_distance_pct(),
```

- [ ] **Step 4: Add `evaluate_exits` method to `impl ReversalStrategy`**

Add after `evaluate_entry`:

```rust
fn evaluate_exits(
    &self,
    ev: &EventWindow,
    spot: f64,
    now: DateTime<Utc>,
    positions: &PositionLedger,
) -> Vec<StrategyDecision> {
    let mut exits = vec![];
    let ptb = match ev.price_to_beat.and_then(|p| p.to_f64()) { Some(p) => p, None => return exits };

    for (token_id, is_up) in [(&ev.up_token, true), (&ev.down_token, false)] {
        let qty = positions.net_qty(token_id);
        if qty <= Decimal::ZERO { continue; }

        // Stop-loss: spot moved too far from price_to_beat in wrong direction
        let dist = (spot - ptb) / ptb;
        let wrong_direction = if is_up { dist < -self.config.stop_distance_pct }
                              else { dist > self.config.stop_distance_pct };
        if wrong_direction {
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("rev_stop_{}_{}", token_id, now.timestamp_millis()),
                deployment_id: String::new(),
                market_id: ev.event_id.clone(),
                token_id: token_id.clone(),
                side: TradeSide::Sell,
                quantity: qty,
                limit_price: None, // market exit
                purpose: IntentPurpose::Exit,
                created_at: now,
            }));
            continue;
        }

        // Take-profit: PM ask has risen above take_profit_ask
        if let Some(quote) = self.quotes.get(token_id) {
            if let Some(ask) = quote.ask.and_then(|a| a.to_f64()) {
                if ask >= self.config.take_profit_ask {
                    exits.push(StrategyDecision::Exit(TradingIntent {
                        intent_id: format!("rev_tp_{}_{}", token_id, now.timestamp_millis()),
                        deployment_id: String::new(),
                        market_id: ev.event_id.clone(),
                        token_id: token_id.clone(),
                        side: TradeSide::Sell,
                        quantity: qty,
                        limit_price: Some(Decimal::try_from(ask * 0.99).unwrap_or(Decimal::ZERO)),
                        purpose: IntentPurpose::Exit,
                        created_at: now,
                    }));
                    continue;
                }
            }
        }

        // Time-stop: < 30s remaining
        let time_remaining = (ev.end_time - now).num_seconds();
        if time_remaining < 30 {
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("rev_time_{}_{}", token_id, now.timestamp_millis()),
                deployment_id: String::new(),
                market_id: ev.event_id.clone(),
                token_id: token_id.clone(),
                side: TradeSide::Sell,
                quantity: qty,
                limit_price: None,
                purpose: IntentPurpose::Exit,
                created_at: now,
            }));
        }
    }
    exits
}
```

- [ ] **Step 5: Call `evaluate_exits` from `handle_spot`**

In `handle_spot`, after collecting entry decisions, add:

```rust
// Check exits for open positions
for ev in &events {
    let exit_decisions = self.evaluate_exits(ev, price_f, *ts, positions);
    decisions.extend(exit_decisions);
}
```

- [ ] **Step 6: Run tests and check compile**

```bash
cargo test -p ploy-strategy-bundles 2>&1 | tail -15
cargo check -p ploy-strategy-bundles --all-targets 2>&1 | tail -10
```
Expected: all tests pass, no compile errors.

- [ ] **Step 7: Commit**

```bash
git add crates/ploy-strategy-bundles/src/strategies/reversal.rs
git commit -m "feat: add take-profit, stop-loss, and time-stop exit logic to ReversalStrategy"
```

---

## Task 8: Config TOML files

**Files:**
- Create: `config/strategies/05-reversal.dryrun.toml`
- Create: `config/strategies/05-reversal.backtest.toml`

- [ ] **Step 1: Create `config/strategies/05-reversal.dryrun.toml`**

```toml
# PM5D Reversal Strategy — dry-run config
# Thresholds are conservative defaults; update after running signal_attribution.

[strategy]
symbols = ["BTCUSDT", "DOGEUSDT"]

# Gate 1: spot must be within 1.5% of price_to_beat
max_distance_pct = 0.015

# Gate 2: reversal signal
max_drift_flip_age_secs = 20
min_post_flip_drift = 0.0001

# Gate 3: LOB confirmation
lob_depth_pct = 0.001
min_lob_depth_ratio = 1.3

# Gate 4: PM mispricing
max_ask_for_reversal = 0.25
max_pm_lag_secs = 30

# Gate 5: time window and edge
min_time_remaining_secs = 60
max_time_remaining_secs = 240
min_edge = 0.05

# Exit thresholds
take_profit_ask = 0.65
stop_distance_pct = 0.025

# Risk
cooldown_secs = 90
stake_usd = "10.0"
allowed_window_secs = [300]
```

- [ ] **Step 2: Create `config/strategies/05-reversal.backtest.toml`**

```toml
# PM5D Reversal Strategy — backtest config
# Looser thresholds to capture more signal for research.

[strategy]
symbols = ["BTCUSDT", "ETHUSDT", "DOGEUSDT", "SOLUSDT", "XRPUSDT"]

max_distance_pct = 0.020
max_drift_flip_age_secs = 30
min_post_flip_drift = 0.00005
lob_depth_pct = 0.001
min_lob_depth_ratio = 1.0
max_ask_for_reversal = 0.35
max_pm_lag_secs = 60
min_time_remaining_secs = 45
max_time_remaining_secs = 270
min_edge = 0.02
take_profit_ask = 0.60
stop_distance_pct = 0.030
cooldown_secs = 60
stake_usd = "10.0"
allowed_window_secs = [300]
```

- [ ] **Step 3: Commit**

```bash
git add config/strategies/05-reversal.dryrun.toml \
        config/strategies/05-reversal.backtest.toml
git commit -m "feat: add ReversalStrategy dryrun and backtest config files"
```

---

## Task 9: Backtest validation

This task is manual — run the backtest, compare against DirectionalStrategy baseline, record findings.

- [ ] **Step 1: Run backtest on clean windows**

```bash
cargo run --release -p ploy-strategy-bundles --example backtest -- \
  --db-url "$PLOY_DB_URL" \
  --config config/strategies/05-reversal.backtest.toml \
  --start-date 2026-04-01 \
  --end-date 2026-04-10 \
  --output /tmp/reversal_backtest.json 2>&1 | tail -20
```

- [ ] **Step 2: Compare against DirectionalStrategy baseline**

Run the same date range with the directional config:

```bash
cargo run --release -p ploy-strategy-bundles --example backtest -- \
  --db-url "$PLOY_DB_URL" \
  --config config/strategies/02-pm5d.v3-live.toml \
  --start-date 2026-04-01 \
  --end-date 2026-04-10 \
  --output /tmp/directional_backtest.json 2>&1 | tail -20
```

- [ ] **Step 3: Record findings in `tasks/todo.md`**

Add a section with:
- Total trades, win rate, P&L for each strategy
- Which gate rejects the most entries (add `tracing::debug!` calls to each gate if needed)
- Whether LOB gate adds value (compare with `min_lob_depth_ratio = 1.0` vs `1.3`)
- Recommended threshold adjustments for live config

- [ ] **Step 4: Commit findings**

```bash
git add tasks/todo.md
git commit -m "research: record reversal strategy backtest findings"
```

---

## Self-Review

**Spec coverage check:**

| Spec section | Covered by task |
|---|---|
| `MarketUpdate::L2Depth` variant | Task 1 |
| `near_depth()` helper + `load_l2_data` extension | Task 2 |
| `signal_attribution` example | Task 3 |
| Attribution analysis (research gate) | Task 4 |
| `ReversalConfig` + state types | Task 5 |
| `on_update` 5-gate pipeline | Task 6 |
| Exit logic (TP, SL, time-stop) | Task 7 |
| Config TOML files | Task 8 |
| Backtest validation | Task 9 |

**Type consistency check:**
- `EventWindow` defined in Task 5, used in Tasks 6 and 7 — fields match.
- `DriftHistory` defined in Task 5, updated in Task 6 `handle_spot` — fields match.
- `LobDepthState` defined in Task 5, updated in Task 6 `on_update` — fields match.
- `TradingIntent` constructed in Tasks 6 and 7 using same field set — consistent.
- `ReversalConfig` extended in Task 7 with `take_profit_ask` and `stop_distance_pct` — both referenced in Task 8 TOML files.

**No placeholders found.**
