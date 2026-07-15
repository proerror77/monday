# Research Pipeline Fixes Design

Date: 2026-04-17

## Problem Summary

Code review identified 6 issues in `ploy-research` and `ploy-strategy-bundles`:

1. **Regime enum split** — two incompatible definitions across crates
2. **IC stability semantic error** — `stability` field stores `ic.abs()`, not ICIR
3. **ReplayBuffer deterministic sampling** — breaks DQN experience replay
4. **Sharpe missing annualization** — not comparable across strategies
5. **Population variance** — `/n` instead of `/(n-1)`
6. **`attach_future_pm_labels` duplication** — two identical functions

---

## Fix 1: Shared Regime in `ploy-operator-contracts`

### What

Move `Regime` to `crates/ploy-operator-contracts/src/trading.rs`. Use the
4-variant definition from `ploy-strategy-bundles` (more granular, matches live).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Regime {
    Early,   // 181..=300s
    Middle,  // 61..=180s
    Late,    //   6..=60s
    Expiry,  //   0..=5s
}

impl Regime {
    pub fn from_secs(t: i64) -> Self {
        match t {
            181..=300 => Regime::Early,
            61..=180  => Regime::Middle,
            6..=60    => Regime::Late,
            _         => Regime::Expiry,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Regime::Early  => "early",
            Regime::Middle => "middle",
            Regime::Late   => "late",
            Regime::Expiry => "expiry",
        }
    }
}
```

### Migration

- `ploy-research/src/factors_new/registry.rs` — delete local `Regime`, add
  `use ploy_operator_contracts::Regime`
- `ploy-strategy-bundles/src/strategies/three_layer.rs` — delete local `Regime`,
  add `use ploy_operator_contracts::Regime`
- Add `ploy-operator-contracts` to `ploy-research/Cargo.toml` dependencies
- Update all `Regime::from_secs` call sites in `ploy-research` (backtest engine,
  attribution, scan) — boundary semantics change from 3-variant to 4-variant

### Boundary change impact

| Old (research, 3-variant) | New (shared, 4-variant) |
|---|---|
| Early: >270s | Early: 181-300s |
| Middle: 61-270s | Middle: 61-180s + Late: 6-60s |
| Expiry: ≤60s | Expiry: 0-5s |

Existing backtest results using the old 3-variant Regime are invalidated and
must be re-run after this change.

---

## Fix 2: IC Stability → ICIR via Event Buckets

### What

`scan_into_registry()` currently sets `stability = ic.abs()` (single-pass IC).
Replace with true ICIR: compute one IC per `event_id` bucket, then
`mean(IC) / std(IC)` across events.

### How

Reuse the existing `bucket_icir()` function from `factors.rs` (already correct).
Build `(event_bucket, x, y)` triples by hashing `event_id` to an `i64` bucket
key, then pass to `bucket_icir(&triples, min_points: 3)`.

```rust
// In scan_into_registry(), replace:
stability: ic.abs(),

// With:
stability: {
    let triples: Vec<(i64, f64, f64)> = regime_obs.iter()
        .filter_map(|o| label_fn(o).map(|y| {
            let bucket = event_bucket_id(&o.event_id);
            (bucket, factor_fn(o), y)
        }))
        .collect();
    bucket_icir(&triples, 3).unwrap_or(0.0)
},
```

`event_bucket_id()` is a simple deterministic hash of the event_id string to i64.

### Semantics

`FactorMeta::stability` now means ICIR (information coefficient information
ratio). A value ≥ 0.5 is considered stable. `FactorRegistry::top_n()` already
sorts by `ic.abs()` — no change needed there; `stability` is available for
downstream filtering.

---

## Fix 3: ReplayBuffer Random Sampling

### What

Replace deterministic evenly-spaced sampling with uniform random sampling using
the `rand` crate (already in workspace).

```rust
pub fn sample(&self, n: usize, rng: &mut impl Rng) -> Vec<&Transition> {
    if n == 0 || self.buffer.is_empty() { return vec![]; }
    let indices = rand::seq::index::sample(rng, self.buffer.len(), n.min(self.buffer.len()));
    indices.iter().map(|i| &self.buffer[i]).collect()
}
```

Callers pass `&mut rand::thread_rng()`. The old deterministic `sample()` is
removed entirely.

---

## Fix 4: Sharpe — Document Per-Trade Semantics

### What

The Sharpe in `BacktestMetrics` is per-trade (not annualized). Rather than
adding annualization (which requires knowing trade frequency), rename the field
and add a doc comment to make the semantics explicit.

```rust
/// Per-trade Sharpe: mean_pnl / std_pnl. Not annualized.
/// To annualize: multiply by sqrt(trades_per_year).
pub sharpe_per_trade: f64,
```

This is a field rename — update all call sites.

---

## Fix 5: Sample Variance in BacktestMetrics

### What

`metrics.rs` line 21: change `/n` to `/(n-1)` for unbiased sample variance.
Guard against `n == 1` (variance undefined → Sharpe = 0).

```rust
let variance = if n > 1 {
    pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1) as f64
} else {
    0.0
};
```

---

## Fix 6: Merge `attach_future_pm_labels` Duplication

### What

`factors.rs` has two identical functions differing only in `horizon_secs` and
target field. Merge into one:

```rust
fn attach_future_pm_labels(rows: &mut [FactorObservation], horizon_secs: i64, field: LabelField)
```

where `LabelField` is a small enum `{ Change30s, Change60s }` selecting which
`Option<f64>` field to write.

---

## Execution Plan

Two parallel agents:

**Agent 1 — Algorithm fixes** (no cross-crate changes):
- Fix 3: ReplayBuffer random sampling
- Fix 4: Sharpe field rename + doc comment
- Fix 5: Sample variance
- Fix 6: Merge attach_future_pm_labels

**Agent 2 — Structural fixes** (cross-crate):
- Fix 1: Add Regime to ploy-operator-contracts, migrate both crates
- Fix 2: IC stability → ICIR via event buckets

Agent 2 must complete Fix 1 before Fix 2 (Fix 2 uses the new Regime boundaries
in scan_into_registry).

---

## Success Criteria

- `cargo test -p ploy-research -p ploy-strategy-bundles` passes
- `cargo check --workspace` passes
- No `Regime` definition remains in `ploy-research` or `ploy-strategy-bundles`
- `FactorMeta::stability` contains ICIR values, not raw IC
- `ReplayBuffer::sample` takes an `rng` parameter
- `BacktestMetrics::sharpe_per_trade` replaces `sharpe`
