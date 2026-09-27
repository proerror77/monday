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

The sequence dataset `source_manifest_sha256` names its PIT snapshot (or the
admitted multi-source PIT index). A fold's `replay_manifest_sha256` names the
validation matching Parquet manifest. These are distinct artifact types and
cannot be compared for equality; training also uses an earlier time interval.
The comparison library pins the exact sequence dataset and fold hashes. The
Campaign admission layer must verify the original PIT receipts and compare
validation source segments against the matching replay before granting any
research/economic outcome. Library fit success alone does not make this claim.

An ensemble also requires identical train-only scaling across both neural seeds.
Its entry policy is constructed from the fitted study identity, so callers
cannot substitute cheaper fees or a later exit boundary while preserving that
identity. Each declared training view must permit at least 64 mature anchors;
validation and sealed views must each permit a mature decision.

## Separate self-supervised encoder route

The two-stage route tracked by [#1235](https://github.com/proerror77/monday/issues/1235)
uses the [market encoder implementation plan](../plans/2026-09-27-sol-market-encoder-pretraining.md).
It does not change the already defined four-group #1230 study or reuse its grant
under different semantics.

`hft-research-manifest::market_encoder` defines separate feature-only and
30-second target datasets. `hft-research-ml::market_encoder` provides bounded,
hash-verified feature readers, causal whole-frame masked reconstruction, an
encoder-only checkpoint, and scratch / frozen linear probe / full fine-tuning
APIs. Adaptation verifies the actual loaded encoder values and fixed train-only
scaling. A frozen probe cannot update encoder parameters; full fine-tuning with
no encoder update is a failed fit. The complete task bundle restores both the
adapted encoder and one return head. Exact JSON floating-point roundtrips and
original Burnpack bytes preserve immutable artifact identity across restore.

## Two-stage study comparison contract

`alpha-domain::market_encoder_study` fixes two 14-day development folds, seeds
7/11, separate independent-selection and sealed identities, and 30-second tasks.
The generated stage list contains pretraining, scratch, linear probe, full
fine-tuning, a compute-control scratch model and Ridge. Each primary stage has a
separate verification stage; downstream inheritance requires its own fold/seed's
primary encoder and verified refit. The plan reserves 22 development plus 4
conditional final primary fits, with matching verification budgets. Checking
remaining capacity requires authenticated cumulative consumption, not an assumed
zero balance.

`alpha-engine::market_encoder_study` binds the fitter and restored artifacts to
that study and stage identity. Learned-value digests compare parameters, feature/target scaling, fit requests
and input identities independently of container IDs and stage purpose.
Task ensembles require both declared seeds and use their mean. The entry policy
shares the existing 30-second IOC cost gate; target labels do not enter an entry
decision. Development prediction coverage remains explicit, and incomplete
coverage prohibits an economic pass. These library contracts do not by themselves
reserve a Campaign, authenticate raw provenance, open holdout or start research.

A production training view also pins `market_training_anchors.v1`: an immutable
index of `(series_id, observed_at_ms)` whose causal context and mature target are
both available. Its derivation never uses target magnitudes. P and all downstream
arms bind the same index, so real gaps do not create unequal training populations
or synthetic labels. The pretraining reader needs the index and features only;
the target file can remain absent. It rejects an index entry missing from the
actual series/context. Evaluation views forbid this filtering and retain their
full expected decision grid.

The materializer's opt-in `--market-encoder-output` exports these feature and
target manifests from the same verified replay. It requires SOLUSDT USD-M,
1s/Top5, a 30-second target and aggregate-trade evidence. Feature rows are built
before target eligibility: an unlabeled tail remains in the feature artifact,
while missing endpoints or recovery gaps omit targets instead of inventing zeros.
Trade buckets use receive-time availability. The report binds both manifests;
the target manifest pins its exact feature dataset. For internal training shards,
`--market-feature-start-received-at-ns` and
`--market-feature-end-received-at-ns` delimit the non-overlapping feature
partition. The raw input can include an earlier warmup and a later 30-second
label tail; neither becomes an extra feature partition. That label-only lookahead
must remain inside the training view; it cannot read validation or sealed data. The last training shard may retain an
unlabeled tail, which lies outside the common supervised anchor range.

### Current implementation state

As of this 2026-09-27 implementation slice, the market encoder route includes
native source preparation and cohort admission, canonical Campaign
freeze/finalize/dispatch, one bounded development Job per fold, stage accounting,
restored-model prediction, native IOC replay and independent settlement readback.
Controlled regressions cover these software contracts. This is not a claim of a
published release, a successful real-data run or economic improvement: **no real
market encoder training has been run at this point**. Current code, CI, merge,
image publication and runtime evidence remain separate delivery states.

The downstream arms are A (scratch), B (frozen encoder/linear probe), C (fine-tune
from P), A_compute (additional supervised compute) and Ridge. Every inherited
stage uses its own fold/seed's independently verified P; B and C branch directly
from P. External manifest and weight hashes, parent identity, loaded parameter
values and training scaling are checked on restore. Neural predictions are the
fixed mean of seeds 7 and 11. Only C can qualify a development fold, and only when
its seed-mean MSE strictly improves on both A and A_compute and the original data
and economic gates pass. A successful control arm cannot replace C. Qualification
of one fold is not qualification of the full study.

The reconstruction head and `market_reconstruction_audit.v2` are auxiliary
artifacts, separate from the reusable encoder. After P's fixed training updates,
the diagnostic reads the same eligible training anchors once and uniformly
samples at most 256, including the endpoints. It uses the fixed epoch-0 mask and
the verified 24-channel registry: 11 price channels, 10 depth-quantity channels
and 3 aggregate-trade channels. Each group records masked standardized MSE for
the trained head and a baseline that holds the last unmasked observation across
a masked run. Sampling identities, counts and bounded CPU forward-work counters
are retained; wall time is recorded separately. These diagnostics read neither
targets nor validation, add no optimizer updates and change no selection rule.
Independent P fits compare the numerical diagnostics as well as parameter and
scaling identities; their wall times need not match.

### Authority, accounting and storage

Each development fold has its **own family and Root**, generated by the typed
`describe-study` projection. A fold reserves 11 primary and 11 verification fits:
**22 charged trials in one Job**. Both Roots are exact members of one cumulative
Study, giving 44 development trials across the two folds. Do not reuse one
family's generation-0 Campaign for both folds. The original ceilings remain
30 primary fits, 30 verification fits and CNY 100, subject to authenticated prior
consumption and current cost commitments. The four conditional final primary
fits and matching verification allocation do not authorize an extra retry or a
final evaluation.

The worker records successful, failed and dependency-skipped stages separately.
Actual attempted-fit counters do not pretend that a skipped fit ran; settlement
retains the full reserved allocation, including failure or cancellation. Root
and Study registration use existing approvals and preserve the ledger's recorded
consumption. Signing metadata does not create an approval, reserve trials or
submit a Job. Re-registration cannot reset usage or revive revoked authority.

Raw data and models stay within the ACK/OSS research boundary. Only small frozen
metadata may cross the established operator signing boundary. Materialized input
and durable work subPaths use task-owned block PVCs. The active ledger and its
integrity key remain on private ACK block storage, never OSS CSI. The worker
input cohort is mounted read-only. Its writable work mount is the exact
`sol-market-encoder-attempts/<operation_id>` subPath. Input and work subPaths must
be disjoint, including when they share a task-owned PVC. The controller owns the
stage signing key and active ledger; neither is mounted into the worker. Verify
PVC UIDs, mounts and actual controller/worker node placement rather than inferring
persistence from a directory name. Required same-node affinity supports the RWO
volume arrangement.

Before each new stage, the worker requests a fresh nonce-bound permit. The native
stage controller rechecks current Root/Study authority, the exact Job/Pod, the
original Job deadline and the completed predecessor records before signing.
Missing, stale, mismatched or timed-out permission blocks the fit. Consumed
permits, start markers, immutable completion receipts and models are persisted
atomically. Complete stages can be restored under their admitted identity without
another fit; a start marker without a complete receipt forbids implicit retraining.
Persistent evidence alone is not automatic recovery on another Spot node, Pod or
Job. Revocation uses the exact Job identity for cancellation; a patch request or
unknown response is not terminal-stop evidence.

### Canonical operating sequence

The command names below follow `alpha-harness`. Supply exact source/image
identities, approved budgets, real input hashes and task-specific control files. This is an ordering
guide, not a prefilled grant or a runnable set of credentials. Ledger registration
and inspection require Linux with `MONDAY_EXECUTION_HOST=ack`.

| Order | Boundary and command | Required result |
| --- | --- | --- |
| 1 | ACK: `mission prepare-fresh-inputs --market-encoder-output`, then `mission prepare-sequence-cohort` | Verified native receipts, bounded non-overlapping feature partitions, common training anchors, a complete validation decision grid and matching replay. Prepare train and development validation only. |
| 2 | ACK: `mission dispatch init-stage-authority` | Public stage authority bound to the real work PVC UID; its private key remains in the controller's private area. |
| 3 | Each fold in ACK: `mission campaign-freeze --stage-authority …` | Frozen typed request and its signing plan after source/cohort checks. |
| 4 | Operator signing boundary: sign the frozen transport actions | Exactly result PUT/GET and bundle PUT/GET for the frozen objects. PUT capabilities must preserve `x-oss-forbid-overwrite:true`. Do not publish signed URLs. |
| 5 | Each fold in ACK: `mission campaign-finalize`, then `mission dispatch describe-study` | Finalized submission plus exact family, policy, execution and horizon bindings for that fold's Root. `describe-study` does not require a pre-existing Root. |
| 6 | Operator signing boundary: `mission dispatch sign-root` for each Root, then `mission dispatch sign-study` | Two distinct signed Roots and one cumulative Study containing both exact members. Use distinct per-fold files. |
| 7 | ACK: `approval record` for both Root approvals and the Study approval; `mission dispatch register-study`; `mission dispatch inspect-study` | Existing effective approvals matched to the signed grants, exact two-entry roots manifest, native registration receipts and authenticated usage readback. Do not replace an existing task ledger or assume zero usage. |
| 8 | Each fold in ACK: `mission dispatch stage-controller --prepare-only` | Bound attempt directories and the required controller affinity label. Establish and verify the labelled controller Pod before submission. This step does not reserve fits. |
| 9 | Each fold in ACK: `mission dispatch submit`, then promptly run `mission dispatch stage-controller` under the same owner | Native reservation/publication, suspended Job creation, actual Job UID binding and release, followed by current per-stage permissions. Do not manually unsuspend or run a second controller for the same attempt. |
| 10 | ACK: `mission dispatch status`, then `mission dispatch settle --input-root … --readback-cache …` | Actual terminal state, fresh remote result identity, exact archive inventory, restored stage/parent models, regenerated forecasts and native cost replay, followed by authenticated settlement. |

The active `campaign-control` retains the existing schema. For this route,
`materialization_path` identifies that fold's cohort-inputs metadata; receipt
capabilities must cover the cumulative Study and the relevant family chain.
Insufficient capabilities stop the same attempt without erasing its reservation.
A task's first ledger can be established by the authorized ACK owner through the
native approval-record path; `register-study` itself accepts only an existing
ledger with its existing file-backed integrity key.

Preparation remains bounded by the frozen part list and original absolute
cutoff. A 14-day hourly partition has 336 parts, not 336 Campaigns or fits.
Completed identical preparation parts may be reused after native readback;
changed windows, producer identities or label tails are different inputs.
Prefix or part counts do not prove complete market coverage. The legacy
`campaign-cycle-controller.sh` wrapper has not been adapted to the market
stage-authority contract; use the canonical native sequence above, without
legacy reuse or diagnostic execution shortcuts.

The independent reader obtains the remote result afresh before trusting cached
bundles, validates every declared file and historical permit, and restores both
primary and verification models and their parents. It compares fitted values,
actual attempt counts, fixed seed-mean predictions, coverage and IOC replay.
Economic evidence retains positions/fills, fees, net return, failed exits,
six-hour blocks and the existing cost/latency stress diagnostics. Cancellation
settlement requires independently confirmed terminal failure and preserves the
full charge; it cannot promote a successful research outcome.

### Remaining outcome boundary

The market encoder **final-holdout adapter is not implemented**. Current workers
remain pre-holdout, do not mount independent-selection or sealed data, and emit
`sealed_holdout_opened=false` and `deployment_authority=false`. Existing generic
final-evaluation machinery is not permission to open these views for this model.
Real preparation coverage, remaining budget, immutable release identities,
actual development training and economic results still require their own runtime
readback. Controlled fixtures establish software behavior only.
