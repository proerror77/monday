# PLOY Native Rust Probability Model Checklist

Historical standalone PLOY deployment instructions are not Monday authority.
The supported model lane is the native Burn binary-probability trainer in
`crates/ploy-research`.

## Dataset contract

- Each observation binds one Polymarket event, symbol, five-minute window, and
  exact decision row from an immutable research snapshot.
- Chainlink remains the opening-reference and expiry semantic authority;
  `official_resolution` is the required settlement-label authority.
- Binance spot, aggTrade, and L2 are predictive inputs only; they cannot replace
  the reference, settlement label, or executable Polymarket price.
- Training and validation event IDs are disjoint. The latest training event
  settlement boundary must be no later than the earliest validation decision.
- Split inputs carry only event ID and decision timestamp selectors. Rust
  projects feature values from a closed, label-free registry against that exact
  content-addressed snapshot row; callers cannot supply feature values, clocks,
  outcomes, or settlement timestamps.
- Snapshot construction and coverage gates enforce point-in-time source
  freshness. The trainer derives the official label and event settlement
  boundary from the governed row; it does not claim to model resolution latency.
- Ordered feature names and their schema hash, mission/snapshot hashes, dataset
  hash, training configuration and seed, and partition counts are bound into
  the typed model manifest.
- Split IDs and purge/embargo fields are not part of the current binary-model
  contract. If they become policy gates, add typed manifest fields and
  fail-closed tests before relying on them.

## Model contract

- Train with native Rust Burn and save a non-overwriting Burnpack artifact with
  trusted manifest and model digests.
- Report out-of-sample validation Brier score, log loss, accuracy, row count,
  and event count. Training-only metrics are never promotion evidence.
- Calibration evidence is a future evaluation gate, not a field emitted by the
  current Burn manifest. Add a typed metric and validation before requiring it.
- Reload the written artifact and reproduce probabilities before accepting the
  bundle.
- The trainer is research-only. It cannot write strategy configuration, approve
  a candidate, submit an intent, or call an execution gateway.

## Validation

Run from `products/ploy`:

```bash
cargo test --locked -p ploy-research --features ml --lib
cargo clippy --locked -p ploy-research --features ml --all-targets --no-deps -- -D warnings
```

Live trading remains disabled. A separately reviewed Monday handoff is required
for any Paper or Shadow use of a governed artifact.
