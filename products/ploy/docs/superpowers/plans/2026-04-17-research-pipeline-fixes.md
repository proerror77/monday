# Research Pipeline Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix 6 correctness and design issues in `ploy-research` and `ploy-strategy-bundles` identified in code review.

**Architecture:** Two parallel tracks — Track A (structural: shared Regime + ICIR) and Track B (algorithmic: metrics, replay buffer, dedup). Track A must complete before Track B's ICIR task since scan.rs depends on the new Regime boundaries.

**Tech Stack:** Rust, `rand = "0.8"` (already in workspace), `ploy-operator-contracts` as shared contracts crate.

---

## File Map

| File | Change |
|---|---|
| `crates/ploy-operator-contracts/src/trading.rs` | Add `Regime` enum (4-variant) |
| `crates/ploy-operator-contracts/src/lib.rs` | Re-export `Regime` |
| `crates/ploy-research/Cargo.toml` | Add `ploy-operator-contracts` dependency |
| `crates/ploy-research/src/factors_new/registry.rs` | Remove local `Regime`, use shared |
| `crates/ploy-research/src/factors_new/scan.rs` | Update regime list (add Late), fix ICIR |
| `crates/ploy-research/src/backtest/engine.rs` | Update Regime import |
| `crates/ploy-research/src/attribution/regime.rs` | Update Regime import |
| `crates/ploy-research/src/signal/regime.rs` | Update Regime import + test boundary |
| `crates/ploy-strategy-bundles/src/strategies/three_layer.rs` | Remove local `Regime`, use shared |
| `crates/ploy-research/src/backtest/metrics.rs` | Sample variance, rename `sharpe_per_trade` |
| `crates/ploy-research/src/attribution/report.rs` | Update `sharpe` → `sharpe_per_trade` |
| `crates/ploy-research/src/model/rl/replay.rs` | Random sampling via `rand` |
| `crates/ploy-research/src/factors.rs` | Merge duplicate `attach_future_pm_labels` |

---

## Task 1: Add `Regime` to `ploy-operator-contracts`

**Files:**
- Modify: `crates/ploy-operator-contracts/src/trading.rs`
- Modify: `crates/ploy-operator-contracts/src/lib.rs`

- [ ] **Step 1: Add Regime to trading.rs**

Append to the end of `crates/ploy-operator-contracts/src/trading.rs`:

```rust
/// Time-remaining regime for a binary option market.
/// Shared between research (backtesting) and live strategy runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Regime {
    /// 181..=300 seconds remaining.
    Early,
    /// 61..=180 seconds remaining.
    Middle,
    /// 6..=60 seconds remaining.
    Late,
    /// 0..=5 seconds remaining.
    Expiry,
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

- [ ] **Step 2: Re-export from lib.rs**

In `crates/ploy-operator-contracts/src/lib.rs`, add:

```rust
pub use trading::Regime;
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo check -p ploy-operator-contracts
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/ploy-operator-contracts/src/trading.rs crates/ploy-operator-contracts/src/lib.rs
git commit -m "feat(operator-contracts): add shared Regime enum (4-variant)"
```

---

## Task 2: Migrate `ploy-research` to shared Regime

**Files:**
- Modify: `crates/ploy-research/Cargo.toml`
- Modify: `crates/ploy-research/src/factors_new/registry.rs`
- Modify: `crates/ploy-research/src/factors_new/scan.rs`
- Modify: `crates/ploy-research/src/backtest/engine.rs`
- Modify: `crates/ploy-research/src/attribution/regime.rs`
- Modify: `crates/ploy-research/src/signal/regime.rs`
- Modify: `crates/ploy-research/src/lib.rs`

- [ ] **Step 1: Add dependency to Cargo.toml**

In `crates/ploy-research/Cargo.toml`, under `[dependencies]`, add:

```toml
ploy-operator-contracts.workspace = true
```

- [ ] **Step 2: Remove local Regime from registry.rs**

In `crates/ploy-research/src/factors_new/registry.rs`, delete the entire `Regime` enum and its `impl` block (lines defining `Early`, `Middle`, `Expiry` variants and `from_secs`, `as_str`).

Replace the top of the file with:

```rust
pub use ploy_operator_contracts::Regime;
```

Keep `FactorMeta` and `FactorRegistry` unchanged.

- [ ] **Step 3: Update scan.rs regime list**

In `crates/ploy-research/src/factors_new/scan.rs`, change line 2:

```rust
use crate::factors_new::registry::{FactorMeta, FactorRegistry, Regime};
```

to:

```rust
use crate::factors_new::registry::{FactorMeta, FactorRegistry};
use ploy_operator_contracts::Regime;
```

Change the regime iteration in `scan_into_registry` from:

```rust
for regime in [Regime::Early, Regime::Middle, Regime::Expiry] {
```

to:

```rust
for regime in [Regime::Early, Regime::Middle, Regime::Late, Regime::Expiry] {
```

Update the test's `obs()` call from `time_remaining_secs: 285` (old Early >270s) to `time_remaining_secs: 220` (new Early 181-300s — still valid) and update the comment:

```rust
// 20 observations in early regime (220s remaining, 181-300s = Early)
let observations: Vec<FactorObservation> = (0..20)
    .map(|i| obs(220, i as f64 * 0.1, if i % 2 == 0 { 1.0 } else { 0.0 }))
    .collect();
```

- [ ] **Step 4: Update backtest/engine.rs import**

In `crates/ploy-research/src/backtest/engine.rs`, change:

```rust
use crate::factors_new::registry::Regime;
```

to:

```rust
use ploy_operator_contracts::Regime;
```

- [ ] **Step 5: Update attribution/regime.rs import**

In `crates/ploy-research/src/attribution/regime.rs`, change:

```rust
use crate::factors_new::registry::Regime;
```

to:

```rust
use ploy_operator_contracts::Regime;
```

Update the test fills to use the new 4-variant Regime. Change `Regime::Early` test fills to use `time_remaining_secs` values in 181-300s range — the fill constructor already takes `Regime` directly so no change needed there.

- [ ] **Step 6: Update signal/regime.rs import and test**

In `crates/ploy-research/src/signal/regime.rs`, change:

```rust
use crate::factors_new::registry::Regime;
```

to:

```rust
use ploy_operator_contracts::Regime;
```

Update the test assertion — old Early was >270s, new Early is 181-300s:

```rust
assert_eq!(router.signal(&obs_at(220)), Signal::Buy);   // early (181-300s)
assert_eq!(router.signal(&obs_at(30)),  Signal::Hold);  // expiry -> falls back to default
```

- [ ] **Step 7: Update lib.rs re-exports**

In `crates/ploy-research/src/lib.rs`, the `Regime` re-export currently comes from `factors_new`. Change:

```rust
pub use factors_new::{FactorMeta, FactorRegistry, Regime, scan_into_registry};
```

to:

```rust
pub use factors_new::{FactorMeta, FactorRegistry, scan_into_registry};
pub use ploy_operator_contracts::Regime;
```

- [ ] **Step 8: Verify tests pass**

```bash
cargo test -p ploy-research 2>&1 | tail -20
```

Expected: all tests pass, no compile errors.

- [ ] **Step 9: Commit**

```bash
git add crates/ploy-research/
git commit -m "refactor(research): migrate to shared Regime from ploy-operator-contracts"
```

---

## Task 3: Migrate `ploy-strategy-bundles` to shared Regime

**Files:**
- Modify: `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`

- [ ] **Step 1: Remove local Regime from three_layer.rs**

In `crates/ploy-strategy-bundles/src/strategies/three_layer.rs`, delete the local `Regime` enum and its `impl` block (the `Early/Middle/Late/Expiry` definition with `from_secs` and `as_str`).

Add at the top of the file:

```rust
use ploy_operator_contracts::Regime;
```

- [ ] **Step 2: Verify ploy-strategy-bundles already depends on ploy-operator-contracts**

```bash
grep "ploy-operator-contracts" crates/ploy-strategy-bundles/Cargo.toml
```

If not present, add to `[dependencies]`:

```toml
ploy-operator-contracts.workspace = true
```

- [ ] **Step 3: Verify tests pass**

```bash
cargo test -p ploy-strategy-bundles 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ploy-strategy-bundles/
git commit -m "refactor(strategy-bundles): migrate to shared Regime from ploy-operator-contracts"
```

---

## Task 4: Fix IC stability → ICIR via event buckets

**Files:**
- Modify: `crates/ploy-research/src/factors_new/scan.rs`

Depends on: Task 2 (new 4-variant Regime must be in place).

- [ ] **Step 1: Add event_bucket_id helper and update scan_into_registry**

In `crates/ploy-research/src/factors_new/scan.rs`, add this helper after the `LABELS` constant:

```rust
fn event_bucket_id(event_id: &str) -> i64 {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    event_id.hash(&mut h);
    h.finish() as i64
}
```

Then in `scan_into_registry`, replace the inner loop body:

```rust
// OLD:
let pairs: Vec<(f64, f64)> = regime_obs.iter()
    .filter_map(|o| label_fn(o).map(|y| (factor_fn(o), y)))
    .collect();
if pairs.len() < MIN_OBS { continue; }
let xs: Vec<f64> = pairs.iter().map(|p| p.0).collect();
let ys: Vec<f64> = pairs.iter().map(|p| p.1).collect();
let ic = spearman_ic(&xs, &ys);
if ic.is_nan() { continue; }
registry.insert(FactorMeta {
    name: factor_name.to_string(),
    regime,
    label: label_name.to_string(),
    ic,
    direction: if ic >= 0.0 { 1 } else { -1 },
    stability: ic.abs(),
});
```

```rust
// NEW:
let triples: Vec<(i64, f64, f64)> = regime_obs.iter()
    .filter_map(|o| label_fn(o).map(|y| {
        (event_bucket_id(&o.event_id), factor_fn(o), y)
    }))
    .collect();
if triples.len() < MIN_OBS { continue; }
let xs: Vec<f64> = triples.iter().map(|t| t.1).collect();
let ys: Vec<f64> = triples.iter().map(|t| t.2).collect();
let ic = spearman_ic(&xs, &ys);
if ic.is_nan() { continue; }
let icir = crate::factors::bucket_icir(&triples, 3).unwrap_or(0.0);
registry.insert(FactorMeta {
    name: factor_name.to_string(),
    regime,
    label: label_name.to_string(),
    ic,
    direction: if ic >= 0.0 { 1 } else { -1 },
    stability: icir,
});
```

- [ ] **Step 2: Make bucket_icir pub(crate)**

In `crates/ploy-research/src/factors.rs`, change:

```rust
fn bucket_icir(bucketed: &[(i64, f64, f64)], min_points: usize) -> Option<f64> {
```

to:

```rust
pub(crate) fn bucket_icir(bucketed: &[(i64, f64, f64)], min_points: usize) -> Option<f64> {
```

- [ ] **Step 3: Update scan test to use multiple event_ids**

The existing test uses a single `event_id: "e1"` for all observations — with one event bucket, `bucket_icir` returns `None` (needs ≥2 buckets). Update the test to use multiple event IDs so ICIR can be computed:

```rust
#[test]
fn scan_populates_registry_for_early_regime() {
    // 20 observations across 5 events in early regime (220s = Early 181-300s)
    let observations: Vec<FactorObservation> = (0..20)
        .map(|i| {
            let mut o = obs(220, i as f64 * 0.1, if i % 2 == 0 { 1.0 } else { 0.0 });
            o.event_id = format!("event-{}", i / 4); // 5 events, 4 obs each
            o
        })
        .collect();
    let mut reg = FactorRegistry::new();
    scan_into_registry(&observations, &mut reg);
    let top = reg.top_n(Regime::Early, "settlement_up", 1);
    assert!(!top.is_empty(), "registry should have at least one early factor");
}
```

Note: `obs()` returns a struct with `event_id` field — mutate it after construction.

- [ ] **Step 4: Run tests**

```bash
cargo test -p ploy-research factors_new 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/factors_new/scan.rs crates/ploy-research/src/factors.rs
git commit -m "fix(research): replace ic.abs() stability with ICIR via event buckets"
```

---

## Task 5: Fix BacktestMetrics — sample variance + rename sharpe

**Files:**
- Modify: `crates/ploy-research/src/backtest/metrics.rs`
- Modify: `crates/ploy-research/src/attribution/report.rs`

- [ ] **Step 1: Update metrics.rs**

Replace the variance and sharpe block in `BacktestMetrics::from_pnls`:

```rust
// OLD:
let variance = pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / n as f64;
let sharpe = if variance > 0.0 {
    mean / variance.sqrt()
} else if mean > 0.0 {
    f64::INFINITY
} else if mean < 0.0 {
    f64::NEG_INFINITY
} else {
    0.0
};
```

```rust
// NEW:
let variance = if n > 1 {
    pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1) as f64
} else {
    0.0
};
/// Per-trade Sharpe: mean_pnl / std_pnl. Not annualized.
/// To annualize: multiply by sqrt(trades_per_year).
let sharpe_per_trade = if variance > 0.0 {
    mean / variance.sqrt()
} else if mean > 0.0 {
    f64::INFINITY
} else if mean < 0.0 {
    f64::NEG_INFINITY
} else {
    0.0
};
```

Rename the struct field from `sharpe: f64` to `sharpe_per_trade: f64` in the struct definition and in the `Self { ... }` constructor at the bottom of `from_pnls`.

Also update the zero-case early return:

```rust
return Self { trade_count: 0, win_count: 0, win_rate: 0.0,
    total_pnl: 0.0, sharpe_per_trade: 0.0, max_drawdown: 0.0 };
```

- [ ] **Step 2: Update attribution/report.rs**

In `crates/ploy-research/src/attribution/report.rs`, change:

```rust
eprintln!("Overall: trades={} win_rate={:.1}% total_pnl={:.4} sharpe={:.3} max_dd={:.4}",
    self.overall.trade_count,
    self.overall.win_rate * 100.0,
    self.overall.total_pnl,
    self.overall.sharpe,
    self.overall.max_drawdown,
);
```

to:

```rust
eprintln!("Overall: trades={} win_rate={:.1}% total_pnl={:.4} sharpe_per_trade={:.3} max_dd={:.4}",
    self.overall.trade_count,
    self.overall.win_rate * 100.0,
    self.overall.total_pnl,
    self.overall.sharpe_per_trade,
    self.overall.max_drawdown,
);
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p ploy-research backtest 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ploy-research/src/backtest/metrics.rs crates/ploy-research/src/attribution/report.rs
git commit -m "fix(research): sample variance in BacktestMetrics, rename sharpe to sharpe_per_trade"
```

---

## Task 6: Fix ReplayBuffer — random sampling

**Files:**
- Modify: `crates/ploy-research/src/model/rl/replay.rs`
- Modify: `crates/ploy-research/Cargo.toml`

- [ ] **Step 1: Add rand to ploy-research Cargo.toml**

In `crates/ploy-research/Cargo.toml`, under `[dependencies]`, add:

```toml
rand.workspace = true
```

- [ ] **Step 2: Replace sample() in replay.rs**

Replace the entire `sample` method:

```rust
// OLD:
/// Deterministic sample: evenly spaced. Replace with random sampling in production.
pub fn sample(&self, n: usize) -> Vec<&Transition> {
    if n == 0 || self.buffer.is_empty() { return vec![]; }
    let step = (self.buffer.len() / n).max(1);
    self.buffer.iter().step_by(step).take(n).collect()
}
```

```rust
// NEW:
pub fn sample(&self, n: usize, rng: &mut impl rand::Rng) -> Vec<&Transition> {
    if n == 0 || self.buffer.is_empty() { return vec![]; }
    let count = n.min(self.buffer.len());
    rand::seq::index::sample(rng, self.buffer.len(), count)
        .into_iter()
        .map(|i| &self.buffer[i])
        .collect()
}
```

Add `use rand;` at the top of the file (remove `#![allow(dead_code)]` if it was only covering the old sample).

- [ ] **Step 3: Add a test**

Append to the `#[cfg(test)]` block (or create one if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::traits::Transition;

    fn t(action: u8) -> Transition {
        Transition { state: vec![0.0], action, reward: 0.0, next_state: vec![0.0], done: false }
    }

    #[test]
    fn sample_returns_correct_count() {
        let mut buf = ReplayBuffer::new(100);
        for i in 0..10u8 { buf.push(t(i)); }
        let mut rng = rand::thread_rng();
        let s = buf.sample(5, &mut rng);
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn sample_does_not_exceed_buffer_size() {
        let mut buf = ReplayBuffer::new(100);
        buf.push(t(0));
        let mut rng = rand::thread_rng();
        let s = buf.sample(10, &mut rng);
        assert_eq!(s.len(), 1);
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p ploy-research model::rl 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ploy-research/src/model/rl/replay.rs crates/ploy-research/Cargo.toml
git commit -m "fix(research): replace deterministic ReplayBuffer::sample with random sampling"
```

---

## Task 7: Merge duplicate attach_future_pm_labels

**Files:**
- Modify: `crates/ploy-research/src/factors.rs`

- [ ] **Step 1: Add LabelField enum and merged function**

In `crates/ploy-research/src/factors.rs`, find the two functions `attach_future_pm_labels` and `attach_future_pm_labels_60`. Replace both with:

```rust
#[derive(Clone, Copy)]
enum LabelField { Change30s, Change60s }

fn attach_future_pm_labels(rows: &mut [FactorObservation], horizon_secs: i64, field: LabelField) {
    let mut grouped: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, row) in rows.iter().enumerate() {
        grouped.entry(row.event_id.clone()).or_default().push(idx);
    }
    for indexes in grouped.values_mut() {
        indexes.sort_by_key(|idx| rows[*idx].tick_ts);
        for (pos, row_idx) in indexes.iter().enumerate() {
            let target_ts = rows[*row_idx].tick_ts + chrono::Duration::seconds(horizon_secs);
            let mut future_change = None;
            for next_idx in indexes.iter().skip(pos + 1) {
                if rows[*next_idx].tick_ts >= target_ts {
                    if rows[*row_idx].pm_up_ask.is_finite() && rows[*next_idx].pm_up_ask.is_finite() {
                        future_change = Some(rows[*next_idx].pm_up_ask - rows[*row_idx].pm_up_ask);
                    }
                    break;
                }
            }
            match field {
                LabelField::Change30s => rows[*row_idx].future_up_ask_change_30s = future_change,
                LabelField::Change60s => rows[*row_idx].future_up_ask_change_60s = future_change,
            }
        }
    }
}
```

- [ ] **Step 2: Update call sites**

Find the two call sites (around line 967-968):

```rust
// OLD:
attach_future_pm_labels(&mut rows, 30);
attach_future_pm_labels_60(&mut rows, 60);
```

```rust
// NEW:
attach_future_pm_labels(&mut rows, 30, LabelField::Change30s);
attach_future_pm_labels(&mut rows, 60, LabelField::Change60s);
```

- [ ] **Step 3: Update the test call site**

Find the test that calls `attach_future_pm_labels` (around line 1853):

```rust
// OLD:
attach_future_pm_labels(&mut rows, 30);
```

```rust
// NEW:
attach_future_pm_labels(&mut rows, 30, LabelField::Change30s);
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p ploy-research 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 5: Final workspace check**

```bash
cargo check --workspace 2>&1 | grep -E "^error" | head -20
```

Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/ploy-research/src/factors.rs
git commit -m "refactor(research): merge duplicate attach_future_pm_labels into single fn with LabelField"
```

---

## Final Verification

- [ ] `cargo test -p ploy-research -p ploy-strategy-bundles -p ploy-operator-contracts` — all pass
- [ ] `cargo check --workspace` — no errors
- [ ] No `Regime` definition in `ploy-research/src/factors_new/registry.rs`
- [ ] No `Regime` definition in `ploy-strategy-bundles/src/strategies/three_layer.rs`
- [ ] `FactorMeta::stability` contains ICIR (not `ic.abs()`)
- [ ] `BacktestMetrics` has `sharpe_per_trade` field (not `sharpe`)
- [ ] `ReplayBuffer::sample` takes `rng: &mut impl rand::Rng`
