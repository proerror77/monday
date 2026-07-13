# pm_5m_directional v3.1 — Signal Strengthening Design

**Date**: 2026-04-11
**Status**: Approved
**Goal**: Tighten the existing V3 directional entry logic so it trades only when
recent spot structure shows both persistent same-direction movement and enough
directional support, without adding any new feed dependencies.

## Problem

The current V3 path already adjusts `base_p` with drift, acceleration, and
directional consistency from the `ReturnBuffer`, but that adjustment is still
soft:

- marginal entries can survive if `base_p` and `edge` are already good enough
- a short burst in the right direction can look tradable even when the last
  few seconds are noisy
- there is no explicit requirement that the most recent spot path stayed aligned
  with the entry direction for any minimum duration

That makes V3 better than V2 structurally, but not yet clearly more selective.

## Design

### 1. Add two V3-only structure thresholds

Extend `DirectionalConfig` with two additive fields:

- `min_trend_consistency`
  - minimum fraction of recent tick moves that must support the entry
    direction
  - default `0.50` so existing non-V3 configs preserve current behavior
- `min_trend_persistence_secs`
  - minimum trailing aligned duration in the return buffer
  - default `0` so existing non-V3 configs preserve current behavior

### 2. Measure signal-direction support explicitly

Reuse the existing `ReturnBuffer` but expose a small helper for trailing
alignment:

- compute `aligned_consistency` from the existing
  `directional_consistency()` output
- compute `persistence_secs` as the consecutive trailing seconds whose returns
  stayed aligned with the entry direction

These metrics stay local to `directional.rs`; no runtime or feed changes are
needed.

### 3. Turn weak structure into a hard gate

In `evaluate_entry(...)`, after `base_p` clears Gate 3 and before edge gating:

- if structure adjustment is enabled but aligned consistency is below
  `min_trend_consistency`, reject the entry
- if trailing aligned persistence is below
  `min_trend_persistence_secs`, reject the entry
- keep the existing odds-ratio structure adjustment for entries that pass

This preserves the current V3 idea but makes it a clearer "trend confirmer"
instead of a mostly soft score multiplier.

## Config Direction

Only the roadmap-named V3 configs should opt into the stronger thresholds in
this slice. V1/V2/V4 keep the compatibility defaults.

Initial V3 tuning target:

- `min_trend_consistency = 0.62`
- `min_trend_persistence_secs = 20`

These values are intentionally modest so the first slice proves selectivity
without collapsing trade count.

## Verification

- unit tests in `directional.rs` prove:
  - strong but weakly-supported structure is rejected
  - strong but non-persistent structure is rejected
  - strong persistent structure still enters
- targeted Rust test run for `ploy-strategy-bundles`
- one local V2 vs V3 comparison run to confirm the change narrows V3 behavior
  rather than broadening it
