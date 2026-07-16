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
Polymarket UP/DOWN depth. It recomputes the in-memory snapshot contract before
training or inference, so a snapshot mutated after contract creation is not
accepted.

An `EventDisjointBinarySplit` contains only `(event_id, decision_at_ms)`
selectors. The trainer resolves each selector to one exact governed snapshot
row, derives its settlement boundary and official label there, and materializes
features through a closed decision-time registry. Callers cannot inject feature
values, outcomes, or timestamps. Each registered feature declares its source
clocks; missing, future, or over-age dynamic evidence fails closed under
`max_feature_age_ms`. The event-static Chainlink opening reference must be known
by decision time but does not expire as a rolling quote.

Train and validation events must be disjoint, duplicate
`(event_id, decision_at_ms)` selectors are rejected, and each training label
must be available before the first validation decision. Label availability is
the latest of the event end, the confirmed Chainlink close, and the official
two-token resolution value's database availability. Epoch, selector, feature,
matrix-cell, and total-work limits are checked before allocation or training.

## Evidence and artifacts

Training is seeded and normalization is fit on the training partition only.
The typed manifest binds the mission, snapshot, ordered feature schema, dataset,
training configuration, sample counts, and validation metrics. The reported
out-of-sample metrics are Brier score, log loss, accuracy, row count, and event
count.

`save_bundle` writes a non-overwriting Burnpack model plus typed JSON manifest.
`load_bundle` requires trusted manifest/model digests, rejects cross-mission or
metadata mismatches, and reproduces probabilities after reload. Public
inference accepts only a governed snapshot, mission, and exact selectors; there
is no public arbitrary numeric-row inference entry point.

## Validation

Run from `rust_hft/prediction-markets`:

```bash
cargo +1.91 test --locked -p ploy-research --features ml --lib
cargo +1.91 clippy --locked -p ploy-research --features ml --all-targets --no-deps
```

The second command is the reproducible repository-baseline Clippy check. This
document does not claim a strict `-D warnings` result while unrelated existing
workspace warnings remain; strict warning enforcement must be reported only
with its actual output and scope.

A separately reviewed Monday handoff is required before any governed model can
enter paper or shadow runtime. PLOY live trading remains disabled.
