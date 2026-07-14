# Factor Research Batch Loader Design

**Date:** 2026-04-13
**Scope:** `crates/ploy-research/examples/factor_research.rs`
**Status:** Approved

## Problem

The current `factor_research` example loads data window-by-window: for each discovered
5-minute window it issues two separate DB queries (`load_from_database_with_options` +
`load_research_lob_snapshots`). Over a SSH tunnel this produces O(N windows) round-trips,
making a 12-window DOGE run take many minutes instead of seconds.

Additionally, three "richer LOB" fields (`microprice_offset_bps`, `depth_far_ratio`,
`depth_acceleration`) were previously computed from placeholder data because `L2Depth`
only carries `bid_depth_near` / `ask_depth_near` — no `best_bid`, `best_ask`, `mid_price`,
or far-depth totals. The `load_research_lob_snapshots` function added in the prior slice
fixes this by querying `binance_lob_ticks` directly for the full JSONB depth bands, but
it is still called per-window.

## Goal

1. Reduce DB round-trips from O(N windows) to O(1) per symbol batch.
2. Ensure richer LOB fields (`microprice_offset_bps`, `depth_far_ratio`,
   `depth_acceleration`, `obi_10`) are populated from real data before any factor
   conclusions are drawn.

## Design

### Two-phase execution

```
Phase 1 — Bulk load (1 DB round-trip per symbol group)
  global_start = min(window.start_time for all windows)
  global_end   = max(window.end_time   for all windows)

  all_updates      = load_from_database_with_options(symbols, global_start, global_end)
  all_lob_snapshots = load_research_lob_snapshots(symbols, global_start, global_end)

Phase 2 — Local window slicing (pure memory, O(N windows))
  for each window:
      updates_slice = binary-search slice of all_updates
                      where ts ∈ [window.start_time, window.end_time]
      lob_slice     = binary-search slice of all_lob_snapshots
                      where ts ∈ [window.start_time, window.end_time]
      build_factor_observations_with_lob(updates_slice, lob_slice, max_quote_age_secs)
```

Both `all_updates` and `all_lob_snapshots` are already sorted by `ts` after loading, so
slicing uses `partition_point` (binary search) — O(log N) per window boundary, not O(N).

### Slice helper

A small private function in `factor_research.rs`:

```rust
fn slice_by_time<'a, T, F>(items: &'a [T], start: DateTime<Utc>, end: DateTime<Utc>, ts_fn: F)
    -> &'a [T]
where
    F: Fn(&T) -> DateTime<Utc>,
{
    let lo = items.partition_point(|x| ts_fn(x) < start);
    let hi = items.partition_point(|x| ts_fn(x) <= end);
    &items[lo..hi]
}
```

Used twice per window: once for `MarketUpdate` (using `update_ts`-equivalent logic),
once for `ResearchLobSnapshot` (using `.ts`).

### Memory budget

Worst case: 3 days × 1 symbol × ~1 LOB tick/sec = ~260,000 LOB rows × ~200 bytes ≈ 50 MB.
Multi-symbol runs multiply linearly. For runs exceeding 7 days or 5+ symbols, a
`--max-load-days` guard can be added later; it is not required for the current use case.

### What does NOT change

- `factors.rs` core logic — untouched
- `build_factor_observations_with_lob` interface — unchanged
- `MarketUpdate` enum — unchanged
- Output format (stderr factor tables) — unchanged
- `load_research_lob_snapshots` — unchanged (called once instead of N times)

## Files

| File | Change |
|------|--------|
| `crates/ploy-research/examples/factor_research.rs` | Replace per-window DB calls with bulk load + local slice |

No other files need to change.

## Verification

- `cargo build -p ploy-research --example factor_research` passes
- `cargo test -p ploy-research` stays green (7 tests)
- A 12-window DOGE run completes materially faster than the per-window baseline
- Factor output for `obi`, `spread_bps`, `depth_imbalance` matches prior run (same data,
  different loading path)
- `microprice_offset_bps`, `depth_far_ratio`, `depth_acceleration` now show non-zero
  variance across observations (confirming real data is flowing)
