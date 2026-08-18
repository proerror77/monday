# Alpha Harness
Rust CLI and libraries for the governed, bounded Loop Engineer research plane. It has no order or trade command and does not depend on execution adapters.

## Packages

- `alpha-domain`: typed missions, candidates, feedback, learning, approvals, and signed envelopes.
- `alpha-store`: DuckDB source of truth with content hashes and append-only journals.
- `alpha-engine`: GP, MCTS, Bayesian search, offline Q-learning, OpenAI-compatible proposals/critics, causal DSL evaluation, checkpointing, and learning.
- `alpha-harness`: Agent-facing CLI.

## Current Capability

| Contract | Status | Current terminal evidence |
| --- | --- | --- |
| CEX Cloud Campaign | Implemented through cloud admission, GP, immutable Factor Bank, deterministic Ridge/CART baselines, and bounded subset MCTS | Create-once Mission/result objects plus independent readback with exact SHA-256 matches |
| Factor-Bank subset MCTS | Implemented | Content-bound checkpoint, add/remove/swap trace, and passing equal-absolute-weight selection or an explicit no-selection result; sealed holdout remains closed |
| Four-stage combination walk-forward | Implemented | A passing subset emits a content-addressed, research-only Signal/Sizing/Risk/Execution artifact with same-protocol Ridge/CART evidence; no selection emits no strategy |
| Event-level L2 replay receipt | Implemented | Canonical event replay emits a content-bound receipt and explicit queue/partial-fill/impact/capacity disclosures |
| Final precommit and sealed holdout | Implemented | Exact precommit, at-most-once sealed opening, immutable receipt, bundle, and promotion lineage |
| Signed Paper/Shadow intake | Implemented | Signed four-stage CEX bundles reach the fail-closed Paper/Shadow boundary; this grants no LiveSmall authority |
| Exact-main CEX run and readback | Pending [#606](https://github.com/proerror77/monday/issues/606) | A fresh credential-free USD-M Mission still needs cloud Runtime and independent result readback |
| Prediction Mission v4 | Implemented for `pipeline_smoke` and deterministic `research_trial` | Authenticated partition readmission plus task-isolated settlement, UP-execution, and DOWN-execution result receipts; no external proposal provider |

`mission dispatch submit` is the CEX operator acceptance seam. It submits one
immutable Campaign request and does not read feature rows, render a Mission, or
run strategy search locally. One ACK Job/Pod performs input admission, Mission
render and readback, research execution, and result readback. Low-level Mission
and LoopRun commands remain diagnostics and implementation surfaces; they are
not alternate evidence paths around this contract.

## Data Mission

```bash
cargo run -p alpha-harness -- data sources

cargo run -p alpha-harness -- data acquire \
  --db var/alpha.duckdb \
  --mission-id data-btc-1 \
  --symbol BTCUSDT \
  --interval 1m \
  --limit 500 \
  --artifact-dir var/datasets
```

The command calls the registered public connector, writes a content-addressed JSONL trace and manifest, persists the full manifest as an immutable DuckDB registry revision, and writes a failure artifact on acquisition failure. Research commands reject a disk manifest that does not exactly match that revision. The acquisition path never substitutes fixtures.

## Research Mission

Create a JSON `ResearchMission`, then run one explicitly selected engine:

```bash
cargo run -p alpha-harness -- mission create \
  --db var/alpha.duckdb --mission mission.json

cargo run -p alpha-harness -- mission run \
  --db var/alpha.duckdb \
  --mission-id mission-1 \
  --engine gp \
  --feature-fields book_imbalance \
  --dataset-manifest var/datasets/dataset-....manifest.json \
  --label-horizon-buckets 1 \
  --observation-frequency-millis 60000

cargo run -p alpha-harness -- mission status \
  --db var/alpha.duckdb --mission-id mission-1

cargo run -p alpha-harness -- candidate list \
  --db var/alpha.duckdb --mission-id mission-1

cargo run -p alpha-harness -- candidate show \
  --db var/alpha.duckdb --mission-id mission-1 \
  --candidate-id mission-1-gp-1
```

`candidate show` prints the candidate artifact plus every stored evaluation as
structured JSON: pass/fail, score, failure reasons, the bound evaluation
protocol, and the full metrics (IC/RankIC/ICIR/RankICIR, positive-IC ratio,
per-fold and aggregate net return, drawdown, turnover, trade count, and the
unannualized per-observation net Sharpe). It is read-only evidence readback;
it grants no promotion or runtime authority.

The production CEX entrypoint consumes a private Campaign submission whose
request binds the exact input objects and SHA-256 values, source revision,
image digest, holdout identity, one search seed, output root, and create-once
Mission/result/claim objects. Signed URL query parameters are transport only;
the query-free objects are part of the Campaign identity.

```bash
cargo run -p alpha-harness -- mission campaign-id \
  --request var/submissions/campaign-request.json

cargo run -p alpha-harness -- mission dispatch submit \
  --submission var/submissions/campaign-submission.json \
  --context monday-research-apne1 \
  --namespace monday-research
```

The submit command creates the immutable Secret before the Job, verifies both
objects by cluster readback, and attaches the Secret to the TTL Job for garbage
collection. It prints identities only, never the signed URLs. The Pod rejects
redirects, input/hash drift, an existing holdout claim, and any Mission or
result readback mismatch before accepting terminal evidence.

One Campaign contains exactly one Mission. That Mission already performs the
bounded GP and subset-MCTS iterations under one multiple-testing family. A
negative result is terminal for the Campaign; it is not silently converted into
another seeded Mission that keeps trying against the same holdout.

The direct `mission execute` and checkpoint-resume surfaces remain diagnostic.
They are not the cloud Campaign production path.

Resume a persisted subset-search checkpoint into a fresh work directory by
adding `--resume-url <checkpoint.json>` and
`--resume-sha256 <checkpoint-content-sha256>` using the checkpoint artifact's
embedded `checkpoint_sha256` field. Restore deterministically replays the
complete trace against the newly reproduced Mission, Factor Bank, baselines,
policies, and research context before accepting any persisted RNG, tree
statistics, evaluation, or selection.
Mission admission rejects search plans whose conservative checkpoint-size bound
cannot fit the same 64 MiB resume transport limit.

The artifact binds one Binance Spot or USD-M instrument and horizon, typed
hypotheses and falsifiers, immutable data/policy identities, the search and
evaluation protocol, and typed prior-evidence references. The command derives
research fields, budgets, and costs from that artifact; they are not accepted
as alternate execute flags. The latest credential-free replay writer currently
admits USD-M only: public reference artifacts bind instrument rules, funding,
and open interest, while fee and rebate values remain explicit, content-hashed
Mission assumptions. Historical account-bound and Spot replay schemas remain
read-only. Unknown fields, Prediction Market fields, action
requests, hash drift, cross-instrument inputs, and same-search exposed-holdout
feedback fail before Mission admission. `operational.submitted_at` is retained
for audit but does not alter the semantic Mission identity. After both baselines
pass, execution searches only canonical Factor Bank subsets through add, remove,
and swap actions, scores mechanically oriented equal-absolute-weight signals on
the research folds, and emits the typed weight policy plus deterministic
checkpoint, trace, and terminal-result artifacts. GP screening and subset search
share one multiplicity correction sized for both bounded candidate families;
only passing subset evaluations are selectable; a terminal search with no
passing subset publishes `selected: null` instead of dropping the negative
result. A passing selection also emits `combination-walk-forward.json`: four
content-addressed stages linked from the immutable Factor Bank through fixed
equal-absolute Signal weights, sign-based bounded Sizing, evaluator-owned Risk
limits, and the exact Binance bucketed-L2 cost/execution assumptions. The same
artifact binds the selected walk-forward metrics and both mandatory baseline
evaluations under one protocol while keeping the holdout unopened and carrying
neither deployment nor order-submission authority. Its selected result is
accepted only after exact checkpoint replay derives the same terminal metadata,
subset, and evaluation. Sealed-holdout evaluation requires the separate governed
precommit boundary. This schema binds prior-evidence identities; later holdout
and Paper/Shadow gates own receipt and signature verification.

For `mission run`, `--feature-fields` is required. Supply comma-delimited fields that are present in the prepared dataset, live-executable, and all belong to the same live event domain. GP and LLM produce validated Formula candidates. The low-level CLI rejects `mcts`; Factor-Bank subset MCTS is owned only by the content-bound `mission execute` seam. `bayesian` and `offline-rl` remain research engines but are rejected before opening mission state because their proposal grammars cannot produce live-executable formulas. LLM requires:

```text
ALPHA_LLM_ENDPOINT
ALPHA_LLM_API_KEY
ALPHA_LLM_MODEL
ALPHA_LLM_PROVIDER  # optional
```

Credentials are process inputs and are never persisted.

For an LLM mission, `objective` and `hypothesis_scope` are the governed research brief; put the curated, source-grounded material synthesis in those fields. `prompt_snapshot_id` records the content-addressed source snapshot but is not an implicit retrieval mechanism. The proposer receives that brief, the registered feature names, the mutable scope, and at most eight prior candidate `keep`/`discard` outcomes. It receives no row labels, raw evaluation metrics, or validator thresholds, and code rejects Formula output unless `mutable_scope` explicitly includes `factor_ast` or `factor_formula`.

## Bounded LoopRun

The durable state contract has the following command shape:

```bash
cargo run -p alpha-harness -- loop run \
  --db var/alpha.duckdb \
  --loop-run-id loop-btc-1 \
  --mission-id mission-1 \
  --engine mcts \
  --dataset-manifest var/datasets/dataset-....manifest.json \
  --label-horizon-buckets 1 \
  --observation-frequency-millis 60000 \
  --target-stage shadow-healthy \
  --max-research-missions 2

cargo run -p alpha-harness -- loop status \
  --db var/alpha.duckdb \
  --loop-run-id loop-btc-1
```

The durable LoopRun still targets the retired Formula-MCTS proposal interface,
so it is not a runnable CEX golden path. The content-bound `mission execute` path
owns Factor-Bank subset MCTS and exact checkpoint resume. GP and LLM remain
available through standalone `mission run` commands but cannot claim that
acceptance path. Bayesian and offline RL are rejected during preflight. Missing
evaluation, holdout, Paper, Shadow, or human evidence pauses LoopRun instead of
fabricating progress; invocation does not bypass stage evidence.

## Prediction-Market Research

`alpha-harness` is also the single Monday transport and evidence entrypoint for
prediction-market research. It does not merge the evaluators: continuous
contracts keep the IC/RankIC/ICIR evaluator above, while binary event contracts
use the event-disjoint Brier/log-loss/calibration/full-depth settlement evaluator
compiled as `monday-prediction-evaluator`.

Build an immutable snapshot from the governed read-only research database and
publish it once:

```bash
MONDAY_RESEARCH_DATABASE_URL='postgresql://...' \
alpha-harness prediction snapshot \
  --work-dir /work/snapshot-btc-001 \
  --result-put-url 'https://signed-oss-put-url' \
  -- \
  --start-date 2026-07-01 \
  --end-date 2026-07-02 \
  --symbols BTCUSDT \
  --optimizer-data-dir /inputs/optimizer \
  --data-audit-report /inputs/prediction-data-audit.json
```

The report returns both the archive SHA-256 and the snapshot contract hash.
Bind the latter into a reviewed BTC- or SOL-only mission revision, then run the
mission from immutable GET URLs and publish one evidence bundle:

```bash
alpha-harness prediction execute \
  --work-dir /work/prediction-btc-001 \
  --mission-url 'https://signed-mission-get-url' \
  --mission-sha256 "$MISSION_SHA256" \
  --snapshot-url 'https://signed-snapshot-get-url' \
  --snapshot-sha256 "$SNAPSHOT_ARCHIVE_SHA256" \
  --result-put-url 'https://signed-results-put-url' \
  --result-readback-url 'https://signed-results-get-url'
```

Workers may add `--snapshot-cache-dir /cache/research-snapshots` to reuse a
read-only archive stored as `<snapshot-sha256>.zip`. The key is the normalized
authenticated SHA-256, not the mission, URL, or a mutable tag. A missing entry
uses `--snapshot-url` unchanged; an existing entry is copied into the attempt's
private input directory and hashed before the runner starts. A digest mismatch
fails closed instead of falling back to the URL.

If the prediction LoopRun pauses, the failed attempt still publishes its state
and evidence. Start a new attempt in a new work directory with a new result PUT URL and add
`--resume-url <previous-results-get-url> --resume-sha256 <previous-bundle-sha>`;
the harness restores only `results/` and refuses non-empty local results, and
the LoopRun revalidates the frozen mission, policy, and snapshot identity before
continuing.

The harness verifies the mission and snapshot outer hashes (and the resume
bundle hash when supplied) before starting the precompiled Rust runner, safely
extracts the snapshot into a new private directory on every retry, preserves
runner evidence with atomic artifact writes, and uses an immutable result PUT.
`execution-evidence.json` records `snapshot_archive_source` as
`verified_cache` for a verified cache hit or `trusted_fetch` for the existing
URL fetch path; both remain bound to the same `snapshot_archive_sha256` trust
anchor.
The runner directly invokes the precompiled prediction
evaluator; the runtime image contains no Cargo or source tree. These commands
have no order, submit, cancel, replace, reconciliation, OMS, or venue-key input.
All production execution remains in Monday `risk-control`,
`execution-gateway`, and `apps/live`.

## Evaluation and Learning

```bash
cargo run -p alpha-harness -- evaluate \
  --db var/alpha.duckdb \
  --mission-id mission-1 \
  --candidate-id candidate-1 \
  --dataset-manifest var/datasets/dataset-....manifest.json \
  --label-horizon-buckets 1 \
  --observation-frequency-millis 60000

cargo run -p alpha-harness -- mission learn \
  --db var/alpha.duckdb \
  --mission-id mission-1 \
  --repeated-failure-threshold 3
```

Only a canonical Formula v3 walk-forward Keep candidate can access the sealed holdout. Candidate generators receive label-free proposal metadata; only the evaluator can read labels. Before position mapping, the evaluator persists per-fold time-series IC, RankIC, ICIR, RankICIR, and positive-IC ratio. After mapping, it persists rows, trades, post-cost edge, drawdown, per-observation net Sharpe, raw score, and adjusted score. The versioned evaluation protocol binds the split, costs, label horizon, observation frequency, fold-IC population-deviation ICIR, and unannualized per-observation population-deviation Sharpe definitions. Sharpe remains unannualized by explicit protocol definition. Label horizon and observation frequency come from the registered, content-validated dataset; their CLI flags are required assertions, not alternate sources of truth. The first run freezes the protocol for the mission and each new checkpoint records its hash, so resume fails before an engine transition if the protocol drifts. Legacy feature manifests without label facts fail closed.

Mission `validator_spec` may override `min_time_series_ic`, `min_time_series_rank_ic`, `min_time_series_icir`, `min_time_series_rank_icir`, and `min_positive_ic_ratio`. It may also pre-register a larger multiple-testing family through `multiple_testing_trials`, but cannot declare fewer trials than its candidate budget. The evaluation protocol, config, and metric hashes are bound into versioned sealed evidence, promotion, and bundle hashes. Old evidence remains readable but cannot be promoted without a valid protocol binding. Repeated failures generate one idempotent follow-up mission and learning directive. Add `--llm-critic` for a bounded real failure explanation.

Runtime feedback and search policy revisions enter through typed JSON:

```bash
cargo run -p alpha-harness -- feedback ingest \
  --db var/alpha.duckdb \
  --record signed-feedback.json \
  --trusted-keys runtime-feedback-trusted-keys.json
cargo run -p alpha-harness -- policy propose --db var/alpha.duckdb --record policy.json
```

Feedback records and JSONL logs must contain `SignedRuntimeAttributionEvent` wrappers. The CLI verifies every content hash, key id, and Ed25519 signature before opening DuckDB; raw or tampered runtime events fail closed. A child search policy is adopted only when its validator score strictly beats its adopted parent.

## Signed Handoff

```bash
cargo run -p alpha-harness -- deployment scope-hash --envelope envelope.json
cargo run -p alpha-harness -- approval record --db var/alpha.duckdb --record approval.json
cargo run -p alpha-harness -- deployment sign \
  --db var/alpha.duckdb \
  --envelope envelope.json \
  --signing-key signing-key.hex \
  --key-id research-signer-1 \
  --output signed-envelope.json
```

The deployment signing key file contains exactly 32 bytes as hex and is never stored in DuckDB or passed to the runtime. Live-small signing requires a persisted `human_live_small` approval whose `scope_hash` matches venue, instruments, and live-small intent. Runtime-owned `policy.json` must independently carry the referenced approval id, class, promotion subject, scope hash, signer, validity window, and revocation state; an envelope signer cannot self-assert an approval id. The runtime still refuses live-small activation until universal order/slippage enforcement and real-venue reconciliation/reduce-only acceptance tests pass.

Native contract models are trained by `hft-research-ml` with Burn. The trainer
requires point-in-time rows, an exact feature order, a content-addressed dataset
manifest, a fixed split identifier, purge/embargo metadata, and a deterministic
seed. It writes a Burnpack artifact plus a typed manifest and is lab-only:
training never creates promotion evidence. ONNX remains a read-only compatibility
ingress for already governed artifacts; `candidate register-onnx` still verifies
the bundle-relative checksum, byte length, tensor schema, preprocessing version,
registered PIT feature matrix, and Rust walk-forward results.

For paper/shadow runtime intake, first obtain current hashes:

```bash
cargo run -p hft-live --no-default-features -- \
  --config config/dev/binance_quotes_only.yaml \
  --deployment-hashes-only
```

Then start `hft-live` with the signed envelope, runtime policy, trusted deployment public keys, nonce ledger, audit log, feedback log, and a separate runtime feedback signing key. `policy.json` is runtime-owned and read-only; its `approvals` array is the independent approval authority. Formula and ONNX files must be relative to and contained by the bundle directory. Runtime verification occurs before startup configuration changes.
