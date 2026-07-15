# Event Dataset Verification Matrix

Status: current Monday validation guide. The two standalone scope-guard scripts
were retired in the Rust-only consolidation because they assumed the former PLOY
repository root and a deleted nested optimization workflow.

The only CI authority for this product is the repository-root
`.github/workflows/ploy-ci.yml`. Run the commands below from the Monday repository
root; none of them grants deployment or execution authority.

## Verification matrix

| Check | Command | PASS condition |
| --- | --- | --- |
| Research crate typecheck | `cargo check --manifest-path rust_hft/prediction-markets/Cargo.toml --locked -p ploy-research --tests` | exits 0 |
| Research crate unit tests | `cargo test --manifest-path rust_hft/prediction-markets/Cargo.toml --locked -p ploy-research --lib` | exits 0 |
| Dataset test discovery | `cargo test --manifest-path rust_hft/prediction-markets/Cargo.toml --locked -p ploy-research --lib -- --list` | the expected dataset/split/manifest tests are present |
| Dataset-focused tests | `cargo test --manifest-path rust_hft/prediction-markets/Cargo.toml --locked -p ploy-research dataset:: --lib -- --nocapture` | exits 0 |
| Conditional export example | `cargo test --manifest-path rust_hft/prediction-markets/Cargo.toml --locked -p ploy-research --example factor_research --features db,polars-export -- --nocapture` | exits 0 when the change touches the export surface |
| Current product retirement contract | `cargo test --manifest-path rust_hft/prediction-markets/Cargo.toml --locked -p ploy --test workspace_runtime_retirement` | exits 0 |

Change scope is reviewed from Monday-root paths rather than enforced by a
standalone branch-name script:

```bash
git diff --name-only <base>...HEAD -- \
  rust_hft/prediction-markets/crates/ploy-research \
  rust_hft/prediction-markets/crates/ploy-feed-loaders \
  rust_hft/prediction-markets/crates/ploy-strategy-bundles \
  rust_hft/prediction-markets/crates/ploy-strategy-runtime \
  rust_hft/prediction-markets/apps/ploy-backtest \
  rust_hft/prediction-markets/apps/ploy-replay \
  rust_hft/prediction-markets/Cargo.toml \
  .github/workflows/ploy-ci.yml
```

The output is evidence to inspect, not an automatic failure: a legitimate typed
dataset integration can cross crate boundaries, but every such change must be
explicitly reviewed and covered by the relevant Rust tests.

## Required evidence

1. No `event_id` appears in more than one split; the split key is event-level.
2. Chronology uses the official event end time and a deterministic ordering key.
3. Event index, manifest, split, and event-summary artifacts are typed and
   content-bound.
4. Observation repricing labels remain distinct from official settlement labels.
5. Sequence preparation does not introduce a conflicting split system; research
   results do not grant execution authority.
6. Any touched feed-loader, strategy, replay, or backtest package receives focused
   regression coverage in addition to `ploy-research` tests.

## Historical evidence boundary

The former verification record for slice `16710042..59f7f775` reported 24
`ploy-research` unit tests and seven dataset-focused tests passing. That record is
historical evidence only; rerun the current commands above for every new change.
