# Governed Burn Binary Research Lane

PLOY's native binary-probability model is a Rust-only, research-only lane in
`ploy-research`. It trains a Burn `0.20.1` linear-logit model and has no order,
deployment, approval, or promotion authority.

## Input contract

`BinaryDatasetContract::from_prediction_snapshot` accepts only a currently
valid BTC or SOL five-minute prediction mission and its bound immutable
`ResearchSnapshot`. Preflight verifies the current Rust prediction policy,
content-addressed snapshot binding, governed Chainlink settlement evidence,
audited Binance source surfaces, mission-symbol isolation, and fresh nonempty
Polymarket UP/DOWN depth.

An `EventDisjointBinarySplit` contains only `(event_id, decision_at_ms)`
selectors. The trainer resolves each selector to one exact governed snapshot
row, derives its settlement boundary and official label there, and materializes
features through a closed decision-time registry. Callers cannot inject feature
values, outcomes, or timestamps. Train and validation events must be disjoint,
and every training settlement must precede the first validation decision.

## Evidence and artifacts

Training is seeded and normalization is fit on the training partition only.
The typed manifest binds the mission, snapshot, ordered feature schema, dataset,
training configuration, sample counts, and validation metrics. The reported
out-of-sample metrics are Brier score, log loss, accuracy, row count, and event
count.

`save_bundle` writes a non-overwriting Burnpack model plus typed JSON manifest.
`load_bundle` requires trusted manifest/model digests, rejects cross-mission or
metadata mismatches, and reproduces probabilities after reload.

## Validation

Run from `products/ploy`:

```bash
cargo +1.91 test --locked -p ploy-research --features ml --lib
cargo +1.91 clippy --locked -p ploy-research --features ml --all-targets --no-deps -- -D warnings
```

A separately reviewed Monday handoff is required before any governed model can
enter paper or shadow runtime. PLOY live trading remains disabled.
