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
- Training and validation event IDs are disjoint. The latest time at which a
  complete official training label was locally observed must be no later than
  the earliest validation decision.
- Every horizon-specific task for an underlying uses the same typed wall-clock
  boundary. Training episodes must settle strictly before it and validation
  episodes must start at or after it, so overlapping event lifetimes and their
  shared Chainlink path cannot cross partitions. The boundary is part of the
  dataset contract, dataset hash, and typed model manifest.
- Split inputs carry only event ID and decision timestamp selectors. Rust
  projects feature values from a closed, label-free registry against that exact
  content-addressed snapshot row; callers cannot supply feature values, clocks,
  outcomes, or settlement timestamps.
- Snapshot v2 atomically reads the exact UP/DOWN token-primary-key pair, its
  complementary official outcome, and when both persisted token versions became
  locally available. Shared market identities must agree, but a numeric event
  ID is not assumed to equal a human-readable market slug. It uses the later of
  `resolved_at` and a content-changing
  `fetched_at` (which also covers legacy resolved rows). Snapshot construction,
  coverage checks, and the trainer reject missing, pre-settlement,
  inconsistent, or post-snapshot clocks.
- The trainer accepts only a `VerifiedBinarySnapshot`: Rust reloads the written
  snapshot, hashes and parses the same captured artifact bytes, and matches its
  `snapshot_contract_hash` against the trusted mission/registry SHA-256 before
  feature materialization. Parquet exports preserve the label clock at
  microsecond precision. Existing v1 snapshots must be rebuilt.
- The actual LoopRun's four walk-forward evaluators use the same availability
  cutoff against the first retained validation decision; they do not bypass
  this rule by calling a non-Burn evaluator.
- Ordered feature names and their schema hash, mission and strong snapshot
  contract hashes, training configuration and seed, and partition counts are
  bound into the typed model manifest.
- The shared time boundary is the current purge contract. There is no separate
  free-form split ID or configurable embargo field; changing that policy
  requires a typed schema change and new fail-closed counterexample tests.
- Binary model manifest schema v3 requires that boundary. Older v2 bundles do
  not contain sufficient cohort evidence and must be retrained; the loader does
  not infer or backfill a boundary from their selected rows.

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

Run from `rust_hft/prediction-markets`:

```bash
cargo test --locked -p ploy-research --features ml --lib
cargo clippy --locked -p ploy-research --features ml --all-targets --no-deps -- -D warnings
```

Live trading remains disabled. A separately reviewed Monday handoff is required
for any Paper or Shadow use of a governed artifact.
