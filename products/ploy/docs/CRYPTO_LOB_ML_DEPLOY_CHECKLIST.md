# PLOY Native Rust Probability Model Checklist

Historical standalone PLOY deployment instructions are not Monday authority.
The supported model lane is the native Burn binary-probability trainer in
`crates/ploy-research`.

## Dataset contract

- Each observation binds one Polymarket event, symbol, five-minute window, and
  point-in-time feature vector.
- Chainlink remains the opening-reference and expiry semantic authority.
- Binance spot, aggTrade, and L2 are predictive inputs only; they cannot replace
  the reference, settlement label, or executable Polymarket price.
- Training and validation event IDs are disjoint. Settlement labels must have
  been available by the declared cutoff.
- Feature order, dataset hash, split ID, seed, purge, and embargo metadata are
  part of the model manifest.

## Model contract

- Train with native Rust Burn and save a content-addressed Burnpack artifact.
- Report validation Brier score, log loss, accuracy, calibration evidence, row
  count, and event count. Training-only metrics are never promotion evidence.
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
