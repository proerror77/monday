# Factor Research Batch Loader Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace per-window DB queries in `factor_research` with a single bulk load + local slice, reducing DB round-trips from O(N windows) to O(1) and ensuring richer LOB fields are populated from real data.

**Architecture:** Load the full time range for all symbols in two queries (one for `MarketUpdate` stream, one for `ResearchLobSnapshot`), then slice both in-memory by window boundaries using binary search. The `build_factor_observations_with_lob` call per window is unchanged — only the data source changes from DB to memory slice.

**Tech Stack:** Rust, `chrono::DateTime<Utc>`, `ploy_strategy_bundles::traits::MarketUpdate`, `ploy_research::ResearchLobSnapshot`

---

## File Map

| File | Change |
|------|--------|
| `crates/ploy-research/examples/factor_research.rs` | Replace per-window DB calls with bulk load + local slice; add `market_update_ts` and `slice_by_time` helpers |

No other files change.

---

### Task 1: Add `market_update_ts` helper

`update_ts` in `database.rs` is private. We need an equivalent in `factor_research.rs` that extracts the sort timestamp from a `MarketUpdate`.

**Files:**
- Modify: `crates/ploy-research/examples/factor_research.rs`

- [ ] **Step 1: Add the import for `MarketUpdate`**

At the top of `factor_research.rs`, add to the existing `use` block:

```rust
use ploy_strategy_bundles::traits::MarketUpdate;
```

- [ ] **Step 2: Add `market_update_ts` function**

Add this private function anywhere before `main`:

```rust
fn market_update_ts(u: &MarketUpdate) -> DateTime<Utc> {
    match u {
        MarketUpdate::SpotPrice { ts, .. }
        | MarketUpdate::AggTrade { ts, .. }
        | MarketUpdate::Quote { ts, .. }
        | MarketUpdate::L2 { ts, .. }
        | MarketUpdate::L2Depth { ts, .. }
        | MarketUpdate::SportsState { ts, .. }
        | MarketUpdate::ReferencePrice { ts, .. }
        | MarketUpdate::Kline { ts, .. } => *ts,
        MarketUpdate::EventDiscovered { end_time, window_secs, .. } => {
            *end_time
                - chrono::Duration::seconds(*window_secs as i64)
                - chrono::Duration::hours(1)
        }
        MarketUpdate::EventExpired { end_time, .. } => *end_time,
    }
}
```

- [ ] **Step 3: Build to confirm it compiles**

```bash
cargo build -p ploy-research --example factor_research 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/ploy-research/examples/factor_research.rs
git commit -m "feat(research): add market_update_ts helper for batch slice"
```

---

### Task 2: Add `slice_by_time` generic helper

A binary-search slice function used for both `MarketUpdate` and `ResearchLobSnapshot`.

**Files:**
- Modify: `crates/ploy-research/examples/factor_research.rs`

- [ ] **Step 1: Add `slice_by_time` function**

Add this private function after `market_update_ts`:

```rust
fn slice_by_time<'a, T>(
    items: &'a [T],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    ts_fn: impl Fn(&T) -> DateTime<Utc>,
) -> &'a [T] {
    let lo = items.partition_point(|x| ts_fn(x) < start);
    let hi = items.partition_point(|x| ts_fn(x) <= end);
    &items[lo..hi]
}
```

- [ ] **Step 2: Build to confirm it compiles**

```bash
cargo build -p ploy-research --example factor_research 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/ploy-research/examples/factor_research.rs
git commit -m "feat(research): add slice_by_time binary-search helper"
```

---

### Task 3: Replace per-window DB calls with bulk load

This is the core change. Replace the `for window in &windows` loop body with bulk load before the loop and in-memory slicing inside the loop.

**Files:**
- Modify: `crates/ploy-research/examples/factor_research.rs`

- [ ] **Step 1: Compute global time range after window discovery**

After the `let windows = ...` block (around line 139), add:

```rust
// Compute global time range covering all windows for bulk load.
let global_start = windows
    .iter()
    .map(|w| w.start_time)
    .min()
    .unwrap_or(start);
let global_end = windows
    .iter()
    .map(|w| w.end_time)
    .max()
    .unwrap_or(end);

// Collect all unique symbols across windows.
let bulk_symbols: Vec<String> = {
    let mut seen = std::collections::HashSet::new();
    windows
        .iter()
        .filter(|w| seen.insert(w.symbol.clone()))
        .map(|w| w.symbol.clone())
        .collect()
};
```

- [ ] **Step 2: Bulk-load updates and LOB snapshots before the loop**

After the global range block, add:

```rust
eprintln!(
    "\nbulk loading {} -> {} for {:?}",
    global_start, global_end, bulk_symbols
);

let all_updates = load_from_database_with_options(
    &pool,
    &bulk_symbols,
    global_start,
    global_end,
    &HistoricalLoadOptions {
        require_official_settlement: true,
        ..Default::default()
    },
)
.await
.expect("bulk historical load failed");

let all_lob_snapshots = load_research_lob_snapshots(
    &pool,
    &bulk_symbols,
    global_start,
    global_end,
)
.await
.expect("bulk lob snapshot load failed");

eprintln!(
    "bulk loaded {} updates, {} lob snapshots",
    all_updates.len(),
    all_lob_snapshots.len()
);
```

- [ ] **Step 3: Replace the per-window DB calls with in-memory slices**

Inside the `for window in &windows` loop, replace the two `load_from_database_with_options` and `load_research_lob_snapshots` calls (and their `eprintln!`) with:

```rust
let updates_slice: Vec<MarketUpdate> = slice_by_time(
    &all_updates,
    window.start_time,
    window.end_time,
    market_update_ts,
)
.to_vec();

let lob_slice: Vec<_> = slice_by_time(
    &all_lob_snapshots,
    window.start_time,
    window.end_time,
    |s| s.ts,
)
.to_vec();

eprintln!(
    "\nwindow {} {} -> {} updates={} lob={}",
    window.symbol,
    window.start_time,
    window.end_time,
    updates_slice.len(),
    lob_slice.len(),
);

let observations =
    build_factor_observations_with_lob(&updates_slice, &lob_slice, max_quote_age_secs);
```

Remove the old `let observations = build_factor_observations_with_lob(...)` line that followed the old DB calls.

- [ ] **Step 4: Build**

```bash
cargo build -p ploy-research --example factor_research 2>&1 | tail -5
```

Expected: `Finished` with no errors.

- [ ] **Step 5: Run tests to confirm nothing regressed**

```bash
cargo test -p ploy-research 2>&1 | tail -10
```

Expected: all tests pass (currently 7).

- [ ] **Step 6: Commit**

```bash
git add crates/ploy-research/examples/factor_research.rs
git commit -m "perf(research): bulk load + local slice replaces per-window DB queries"
```

---

### Task 4: Smoke-test against real DB

Verify the refactored loader produces the same factor output as before and that richer LOB fields now have non-zero variance.

**Files:**
- No code changes — this is a verification task.

- [ ] **Step 1: Run a small window against the remote DB**

```bash
target/debug/examples/factor_research \
  --db-url 'postgresql://postgres:postgres@localhost:55432/ploy' \
  --discover-valid-5m-windows \
  --max-windows 3 \
  --start-ts 2026-04-09T00:00:00Z \
  --end-ts 2026-04-10T00:00:00Z \
  --symbols DOGEUSDT
```

Expected output includes:
- `bulk loading ... for ["DOGEUSDT"]`
- `bulk loaded N updates, M lob snapshots` (M > 0)
- Per-window lines showing `lob=K` with K > 0
- Settlement and PM Lag factor tables printed at the end

- [ ] **Step 2: Confirm richer LOB fields have variance**

In the factor tables, check that `microprice_offset_bps`, `depth_far_ratio`, and `depth_acceleration` appear with non-zero `mean_n` and non-zero `spearman` values (positive or negative). If they show `0.0000` for both pearson and spearman, the JSONB depth bands are not being parsed — investigate `binance_lob_ticks.bids` / `binance_lob_ticks.asks` column format.

- [ ] **Step 3: Run a larger multi-window batch and confirm speed improvement**

```bash
time target/debug/examples/factor_research \
  --db-url 'postgresql://postgres:postgres@localhost:55432/ploy' \
  --discover-valid-5m-windows \
  --max-windows 12 \
  --start-ts 2026-04-09T00:00:00Z \
  --end-ts 2026-04-12T00:00:00Z \
  --symbols DOGEUSDT
```

Expected: completes materially faster than the per-window baseline (which was timing out or taking many minutes for 12 windows).

---

## Self-Review

**Spec coverage:**
- ✓ O(N) → O(1) DB round-trips: Tasks 1–3
- ✓ Richer LOB fields from real data: Task 4 verification step 2
- ✓ `build_factor_observations_with_lob` interface unchanged: Task 3 step 3 reuses it directly
- ✓ Only `factor_research.rs` changes: confirmed in file map

**Placeholder scan:** None found.

**Type consistency:**
- `market_update_ts` defined in Task 1, used in Task 3 — consistent
- `slice_by_time` defined in Task 2, used in Task 3 — consistent
- `all_lob_snapshots` is `Vec<ResearchLobSnapshot>` (returned by `load_research_lob_snapshots`) — `lob_slice` is `Vec<ResearchLobSnapshot>` — matches `build_factor_observations_with_lob` second parameter `&[ResearchLobSnapshot]` ✓
