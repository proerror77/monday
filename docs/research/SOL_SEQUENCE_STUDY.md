# SOL existing-data sequence study

Tracking: [#1230](https://github.com/proerror77/monday/issues/1230).

The approved first study uses existing Binance USD-M SOLUSDT LOB/aggregate-trade
data, recent 7/14-day training windows, same-information Ridge/MLP and price-only
versus full-input TCN comparisons. The primary prediction and holding horizon is
30 seconds; 5/10-second outputs are diagnostics. There is no 70-day prerequisite,
no Transformer/pretraining/RL/maker or live activation in this study.

## Current code boundary

`hft-research-manifest::sequence` binds ordered channels, observation clocks,
source identity, immutable bounded shards and half-open views. `hft-research-ml`
provides a bounded sequence reader and native CPU TCN/flattened-MLP trainers. These are library
capabilities, not a completed Campaign or an executable trading candidate.

Each JSONL frame retains a dataset-wide series identity, observation and maximum
feature availability times, ordered finite channels, and fractional simple mid
returns plus maturity clocks for 5/10/30 seconds. Decision-time spread is retained
separately for the cost gate, never appended to the price-only model input.
A context cannot cross a series
change or a missing observation. All three labels must mature strictly before the
view ends. Missing labels cannot be encoded as zero. A production materializer
must bind these rows to verified tape and rule evidence; arbitrary matching
JSONL files do not establish market-data admission.

The reader verifies declared sizes and hashes before fitting, pins file handles,
and verifies bytes, row counts and endpoint clocks again on each pass. It holds
one context and bounded batches instead of materializing all overlapping windows.
Dataset shards exposed to a worker must belong to that worker's authorized view:
the reader checks complete shard bytes and cannot serve as a secrecy barrier for
a sealed shard mounted in the same input dataset.

The MLP flattens the exact same normalized context used by the TCN; it does not
receive a smaller information set. Both use the same mini-batch and scaling path.
The TCN has five causal kernel-3 layers with dilations 1/2/4/8/16, a maximum
63-row receptive field, and a three-return output. Its hidden width, channel
ablation, batch size, seed, update budget, input identity and view are bound in the
request. Explicit left padding preserves causal convolution. Scaling is fitted
only on mature training examples, and inference restores fractional return units.
When the update budget samples less than an epoch, deterministic even sampling
covers the entire training window rather than only its earliest rows.

Training is fixed-budget, not certified converged. Diagnostics retain pre-update
batch losses, example visits and decision range, raw global gradient norms and
the exact stop reason. Nonfinite or excessive loss/gradients fail; a shared global
gradient clip is checked after application. The final parameters must be finite.
Train-only loss controls do not inspect independent validation or sealed labels.

Bundles bind request, scaling, diagnostics and Burnpack weight hash into an
externally pinned manifest hash. Inference uses the same frozen NdArray model;
this does not extend the existing two-layer portable MLP arithmetic certificate
to convolutions or certify arbitrary GPU/mixed-precision kernels.

## Predeclared comparison

`alpha-domain::sequence_study` binds four model groups, seeds 7/11, two
development folds with 7/14-day training windows, a separate sealed identity,
shared validation inputs and the original taker/capacity cost contract. An
explicit decision stride bounds training anchors without dropping intervening
context rows; validation retains every eligible second. Neural budgets must
permit at least one complete pass through the declared maximum anchor count.
Development permits 28 primary model pipelines and at most two final refits;
each pipeline predicts all three horizons, with only 30 seconds eligible for
strategy selection. Verification work has a separate explicit bound.

`alpha-engine::sequence_study` fits Ridge on the same flattened context as the
MLP/TCN family. Exact-constant standardized Ridge columns can be removed from the
solve without changing the problem. A neural candidate requires both declared
seeds and averages their forecasts; it cannot select the best seed. Model
bundles bind the study, fold, dataset, preprocessing, model kind and fitted
values. Parameter digests omit container metadata and random parameter IDs so
independent refits compare the actual learned values.

Prediction ledgers retain the decision-time spread and all three targets.
`complete_decision_grid=false` prohibits an economic pass after a gap, missing
context or immature target removed an expected decision. A future data outage
must not silently erase a preceding trade. The new research policy
`sol-fixed-notional-after-native-cost-gate-v1` uses the existing two-sided cost
gate but proposes a fixed signed notional after entry; the native horizon replay
still owns actual positions, fills and exits. Future labels never enter that
decision. Declared non-entry tail ticks remain necessary to let raw-book replay
close the last position at expiry.

## Remaining study joins

- Export sequence channels and multi-horizon targets from verified PIT replay.
- Bind sequence datasets, model families and budgets through canonical Campaign
  freeze/finalize/dispatch/execute and independent readback. H1/H2 contracts and
  historical failed budgets remain unchanged.
- Wire the four-group comparison library and its immutable results into the
  admitted worker, with verification work explicitly accounted separately.
- Use an unexposed final interval and complete IOC holding replay, including
  fees, depth, latency and failed exits; preserve negative/insufficient outcomes.

The 2026-09-26 metadata audit found daily LOB+trade prefixes from September 1
through 26. Hour prefixes were present throughout September 2–23; September 24
ended at hour 16 and September 25 resumed at hour 11. Prefix presence is not
continuous SOL coverage. A sampled September 25 manifest declared SOLUSDT,
`depth@100ms` and `aggTrade`. Full symbol/sequence/byte admission remains pending.

## Validation

From `rust_hft`, run package-scoped `cargo test --locked -p
hft-research-manifest -p hft-research-ml` and scoped Clippy. Regression checks
cover feature availability, label maturity, gap/session resets, immutable shard
identities, future-prefix invariance, actual delayed-context convolution, finite
raw-return learning, deterministic refitting and tamper-resistant weight/scaling
roundtrips. Controlled fixtures establish software behavior, never market alpha.
