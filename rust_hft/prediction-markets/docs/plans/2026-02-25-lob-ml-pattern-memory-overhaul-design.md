# LOB ML + Pattern Memory Full Overhaul Design

**Date:** 2026-02-25
**Status:** Approved
**Scope:** 12 optimizations across LOB ML (ONNX) and Pattern Memory strategies

---

## Context

Code review identified 12 issues across the two ML-driven crypto strategies:
- LOB ML: 3-layer blend redundancy, LaggingOnly side bug, unnormalized features, model versioning
- Pattern Memory: short patterns (N=10), weak priors, unbounded samples, no time decay, 15m filter bug

### Design Decisions

| Decision | Choice |
|----------|--------|
| Pattern Memory fate | Keep independent, strengthen thresholds |
| Feature normalization | Embed scaler in CryptoLobMlConfig |
| LOB ML blend layers | Unify from 3 to 2 (model + GBM anchor) |
| Time decay | Configurable `age_decay_lambda` parameter |

---

## 1. LOB ML: 2-Layer Blend Refactor

### Problem

The current 3-layer blend has near-duplicate GBM layers:
```
p_final = (1 - w_threshold) × [w_model × p_model + w_window × p_window]
         + w_threshold × p_threshold
```

When `price_to_beat = start_price` (common in UP/DOWN markets), `p_threshold ≈ 1 - p_window`,
causing partial cancellation. Model effective contribution is only 58.5%.

### New Architecture

```
p_final = w_model × p_model + (1 - w_model) × p_gbm_anchor
```

Where `p_gbm_anchor` is computed as:
- **With price_to_beat**: `1 - Φ(required_return / σ_remaining)` where `required_return = (price_to_beat - spot) / spot`
- **Without price_to_beat** (fallback): `Φ(window_move / σ_remaining)` where `window_move = (spot - start_price) / start_price`

Default: `w_model = 0.80`

### Changes

**File: `src/agents/crypto_lob_ml.rs`**

1. Delete `estimate_p_up_window()` (~25 lines)
2. Delete `estimate_p_up_threshold_anchor()` (~40 lines)
3. New function `estimate_p_up_gbm_anchor(spot, start_price, price_to_beat, sigma_1s, remaining_secs) -> Decimal`
4. Rewrite `compute_blended_probability()` to accept 2 inputs instead of 3
5. Remove `p_up_window` from `WindowContext` struct
6. Remove config fields: `window_fallback_weight`, `threshold_prob_weight`
7. Add config field: `model_blend_weight: Decimal` (default `0.80`)

---

## 2. LOB ML: LaggingOnly Fix

### Problem

`LaggingOnly` policy selects entry side purely by ask price comparison, ignoring model direction.
If model says UP (p_up=0.70) but down_ask < up_ask, it buys DOWN.

### Fix

```rust
CryptoLobMlEntrySidePolicy::LaggingOnly => {
    let model_dir = if p_up_blended >= dec!(0.50) { Side::Up } else { Side::Down };
    let (dir_token, dir_ask, dir_edge, dir_gross, dir_fair) = match model_dir {
        Side::Up => (up_token_id, up_ask, up_edge_net, up_edge_gross, p_up_blended),
        Side::Down => (down_token_id, down_ask, down_edge_net, down_edge_gross,
                       Decimal::ONE - p_up_blended),
    };
    // Only enter when model-direction ask is below midpoint (cheap side)
    if dir_ask > dec!(0.50) {
        return None;
    }
    (model_dir, dir_token.to_string(), dir_ask, dir_edge, dir_gross, dir_fair)
}
```

**File: `src/agents/crypto_lob_ml.rs`** lines 1049-1068

---

## 3. LOB ML: Feature Normalization

### Problem

11 sequence features span vastly different scales: `spot_price ~95,000` vs `obi_5 ~0.1` vs
`remaining_secs ~300`. Raw values fed to ONNX model without normalization.

### Design

Add per-feature affine transform `(value - offset) * scale` embedded in config:

```rust
// CryptoLobMlConfig additions:
/// Per-feature offsets for normalization. Length must equal SEQ_FEATURE_DIM (11).
#[serde(default)]
pub feature_offsets: Vec<f32>,
/// Per-feature scales for normalization. Length must equal SEQ_FEATURE_DIM (11).
#[serde(default)]
pub feature_scales: Vec<f32>,
```

**Feature order** (matching SEQ_FEATURE_DIM):
```
[0] obi_5, [1] obi_10, [2] spread_bps, [3] bid_volume_5, [4] ask_volume_5,
[5] momentum_1s, [6] momentum_5s, [7] spot_price, [8] remaining_secs,
[9] price_to_beat, [10] distance_to_beat
```

**In `build_sequence()`**: If offsets/scales are empty or wrong length, fall back to identity
(no normalization) for backward compatibility.

**Training script**: Export scaler to JSON, paste into deploy config.

---

## 4. Pattern Memory: Statistical Strengthening (6 items)

### 4a. Pattern Length N=10 → N=20

**File: `src/strategy/pattern_memory/strategy.rs`**
```rust
const PATTERN_LEN: usize = 20;  // was 10
```

Longer patterns dramatically reduce false correlation matches. At N=20 (df=18),
random Pearson corr > 0.70 probability drops from ~1.2% to ~0.01%.

Cost: cold start requires 20 bars × 5min = 100 minutes instead of 50.

### 4b. Stronger Evidence Thresholds

```toml
[pattern]
min_matches = 10    # was 3
min_n_eff = 5.0     # was 2.0
min_confidence = 0.60  # unchanged
```

### 4c. Stronger Prior: Beta(5,5)

```toml
[pattern]
alpha = 5.0   # was 1.0
beta = 5.0    # was 1.0
```

With 3 all-UP matches: Beta(1,1) → P(UP)=80%, Beta(5,5) → P(UP)=61.5%.

### 4d. Time Decay (configurable)

**File: `src/strategy/pattern_memory/engine.rs`**

Add `timestamp: DateTime<Utc>` to `PatternSample<N>`.

In `posterior_for_required_return()`, add `age_decay_lambda: f64` parameter (from config):
```rust
let age_minutes = now.signed_duration_since(s.timestamp).num_minutes() as f64;
let decay = (-age_decay_lambda * age_minutes).exp();
let w = corr_weight(corr, corr_threshold) * decay;
```

**Config:**
```toml
[pattern]
age_decay_lambda = 0.001  # ~17 hour half-life; 0 = no decay
```

### 4e. Samples Capacity

```rust
pub struct PatternMemory<const N: usize> {
    max_samples: usize,  // default 2000 (~7 days of 5m bars)
    // ...
}
```

On `ingest_return()`, if `samples.len() > max_samples`, remove oldest.

### 4f. 15m Filter required_return Fix

**File: `src/strategy/pattern_memory/strategy.rs`**

Change `filter_15m_ok()` to accept `required_return: f64` parameter instead of hardcoding `0.0`.

---

## 5. LOB ML: Model Version Metadata

### Config Additions

```rust
pub model_sha256: Option<String>,
pub model_trained_at: Option<String>,  // ISO8601
pub model_auc: Option<f64>,
```

On startup: if `model_path` exists, compute SHA256 and compare with `model_sha256`.
Mismatch → `warn!()` (not block). All three fields are recorded in order metadata for
post-hoc performance analysis by model version.

---

## 6. Pattern Memory Walk-Forward Backtest

**New file: `src/analysis/pattern_memory_backtest.rs`**

```
Flow:
1. Read binance_klines (5m + 15m) from DB, sorted by time
2. Walk-forward: each bar → ingest_return(), query posterior
3. If posterior passes trade thresholds → record prediction
4. Compare against pm_token_settlements settlement outcome
5. Output: hit rate, AUC, Brier score, sliced by symbol / required_return
```

Strict walk-forward: each prediction uses only samples from before that timestamp.

---

## 7. Python: Scaler Export + Feature Importance

### Scaler Export

All 5 training scripts gain `--export-scaler <path.json>` flag that writes:
```json
{
  "feature_offsets": [0.0, 0.0, ...],
  "feature_scales": [1.0, 1.0, ...],
  "feature_names": ["obi_5", "obi_10", ...]
}
```

### Feature Importance

TCN/MLP scripts gain `--feature-importance` flag using sklearn permutation importance
on validation set. Outputs ranked feature importance to stdout and JSON.

---

## Implementation Plan

| Phase | Commit | Content | Verification |
|-------|--------|---------|--------------|
| A | 1 | LOB ML: 2-layer blend refactor | `cargo check` + existing tests |
| B | 1 | LOB ML: LaggingOnly fix | `cargo check` |
| C | 1 | LOB ML: Feature normalization in config | `cargo check` + new unit test |
| D | 1 | Pattern Memory: N=20, Beta(5,5), min_matches=10, min_n_eff=5.0 | `cargo test pattern_memory` |
| E | 1 | Pattern Memory: time decay + samples cap + PatternSample timestamp | `cargo test pattern_memory` |
| F | 1 | Pattern Memory: 15m filter required_return fix | `cargo check` |
| G | 1 | LOB ML: model version metadata (sha256, trained_at, auc) | `cargo check` |
| H | 1 | New `pattern_memory_backtest.rs` | `cargo check` |
| I | 1 | Python: scaler export + feature importance | `python --help` |

**Total: 9 atomic commits, 12 optimizations.**

### Verification Criteria

1. Every commit compiles independently (`cargo check`)
2. `cargo test --lib` passes (including new tests)
3. After Phase C: verify normalized sequence output with known values
4. After Phase D+E: Pattern Memory engine tests verify new defaults
5. After Phase H: backtest runs on tango-2-1 production data
