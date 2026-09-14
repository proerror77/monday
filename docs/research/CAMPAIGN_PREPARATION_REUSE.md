# Reusable Campaign preparation

`alpha-harness mission campaign-prepare --plan PLAN.json --output-root DIR`
prepares a bounded matrix in ACK. It performs no signing, ledger reservation,
Job creation or training. The existing Campaign freeze/finalize/dispatch/execute
boundary still owns execution. Parameter-only research changes use the same
published source/image; they do not require changing the controller program.

The versioned plan `monday.cex_campaign_preparation_plan.v1` contains:

| Field | Meaning |
|---|---|
| `source_revision`, `image` | Exact executing source and digest-pinned runner image |
| `campaign_root` | Immutable result-object prefix accepted by native Campaign validation |
| `campaign_inputs` | `{path, sha256}` for the materializer's original receipt bytes |
| `input_root` | ACK-local location of the receipt's input files for first admission |
| `prepared_inputs` | Optional `{path, sha256}` for a previously returned input receipt |
| `seeds` | Two to sixteen distinct native u64 seeds |
| `base_research_plan` | One complete, existing typed native research plan shared by members |
| `members` | One to thirty-two distinct configurations, each with a safe `id` |

A member can supply a complete `research_plan` when needed. For ordinary MLP
comparisons, `mlp: {updates, learning_rate}` changes only those two fields of the
base plan. Paired initializations, factor identities and stability controls stay
in the shared base plan. Unsupported settings, missing initialization/control
contracts, duplicate configurations or invalid plans reject before input
preparation. This does not add model families or select a training policy for
the researcher.

The preparer validates the exact feature bytes once, retains their parsed rows
for replay/availability checks, and releases them after constructing an immutable
metadata snapshot. Materialization is hashed and decoded from one read. The
canonical replay validator owns replay hashes and structure checking; the caller
does not prehash the same files again. All members render from that one snapshot.

The output is a plan-identity directory containing `preparation.json`, one shared
`campaign-inputs.json`, a content-addressed shared input receipt, and native
`research-plan.json`/`freeze.json` per member. The index gives exact artifact
hashes and trial previews. Seeds in the orchestration index are decimal strings
so JSON tools cannot round u64 values; native research plans and frozen request
bytes retain their native serialization.

An identical plan validates and reuses the retained index and artifacts without
opening bulk input files. A durable `input-ready.json` checkpoint lets a partially
completed preparation reuse its already admitted input snapshot. Artifacts are
published atomically without overwriting existing evidence, and a file lock
rejects simultaneous preparation writers. Work interrupted before an input
checkpoint commits has not completed input admission and may need to perform
that stage again.

For a changed parameter matrix, set `prepared_inputs` to the previous index's
input receipt and its independently retained SHA. Reuse checks the receipt hash,
source, image, original input receipt and metadata identities. The old preparation
is preserved; new member requests are rendered without reopening raw features or
replay files. This receipt is a trusted preparation output, not an arbitrary
self-authored cache marker. Worker-side independent input admission is retained.

## Starting from prepared artifacts

Pass the selected member's native research plan, freeze and digest to the existing
`campaign-cycle-controller.sh start` with its normal source/image/input/signing
and control arguments:

```text
--initial-research-plan <member research-plan.json>
--prepared-freeze <member freeze.json>
--prepared-freeze-sha256 <the index's exact freeze SHA256>
```

The controller retains the prepared freeze under its durable work directory and
binds the digest into controller state. The normal native `campaign-freeze` call
uses `--reuse` and `--reuse-sha256`; it checks the frozen request against the
requested receipt, source, image, plan, seeds, output identities and signing plan
without rescanning bulk inputs. Finalization and dispatch are unchanged. The
same option cannot substitute for final holdout admission or fresh-data selection.

For the prepared generation, `approve`/`ack-readback` reuse retained state and
completed checkpoints without the original prepared-freeze file or old bulk-input
directory. Later learning generations still require their own input preparation. Changed
retained bytes reject before another dispatch. Existing deadlines, grants,
failed charges and Job identity checks remain in force.

These interfaces provide reusable preparation and native startup. They do not
by themselves claim that a multi-member executor or a real cloud experiment has
completed. Runtime adoption still requires its own bounded run and readback.

Preparation is not research admission or permission to run. In particular, the
planned H1 comparison must first pass its declared label-amplitude screen on a
verified unseen calendar, before producing any freeze. Its Ridge model, 30-second
`forward_mid_return`, existing factors and cost gate remain fixed. A failed
pre-fit screen is retained negative evidence and must not trigger more models,
automatic cost-policy changes or same-window trials. This preparation interface
alone does not implement that screen or start H1.
