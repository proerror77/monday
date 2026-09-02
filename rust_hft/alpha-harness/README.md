# Alpha Harness
Rust CLI and libraries for the governed CEX Campaign and prediction research plane. It has no order or trade command and does not depend on execution adapters.

## Packages

- `alpha-domain`: typed missions, candidates, feedback, learning, approvals, and signed envelopes.
- `alpha-store`: DuckDB source of truth with content hashes and append-only journals.
- `alpha-engine`: GP, MCTS, Bayesian search, offline Q-learning, OpenAI-compatible proposals/critics, causal DSL evaluation, checkpointing, and learning.
- `alpha-harness`: Agent-facing CLI.

## Current Capability

| Contract | Status | Current terminal evidence |
| --- | --- | --- |
| CEX Cloud Campaign | Implemented through cloud admission, immutable shared-input download, bounded multi-round Campaign execution, continuous GP, immutable Factor Bank, purged walk-forward Ridge/CART, and cost-aware model replay | Create-once per-round Mission/result objects, typed OOS/equity/replay feedback, deterministic pre-holdout winner selection, and independent readback with exact SHA-256 matches |
| Supervised ML research lane | Implemented for governed GP v4 | Continuous factors feed Ridge, shallow CART, and a Burn MLP; only OOS predictions create fractional target positions, and the same positions must pass canonical L2 event replay before selection |
| Legacy Factor-Bank subset MCTS | Preserved for governed GP v1-v3 | Content-bound checkpoint, add/remove/swap trace, and passing equal-absolute-weight selection or an explicit no-selection result; sealed holdout remains closed |
| Legacy four-stage combination walk-forward | Preserved for governed GP v1-v3 | A passing subset emits a content-addressed, research-only Signal/Sizing/Risk/Execution artifact with same-protocol Ridge/CART evidence; no selection emits no strategy |
| Event-level L2 replay receipt | Implemented | Canonical event replay emits a content-bound receipt, net-return/Sharpe gates, and explicit queue/partial-fill/impact/capacity disclosures |
| Final precommit and sealed holdout | Preserved for the legacy formula lane only | The ML v4 lane stops pre-holdout and carries no deployment or order authority |
| Signed Paper/Shadow intake | Implemented | Signed four-stage CEX bundles reach the fail-closed Paper/Shadow boundary; this grants no LiveSmall authority |
| Exact-main CEX run and readback | Pending [#606](https://github.com/proerror77/monday/issues/606) | A fresh credential-free USD-M Mission still needs cloud Runtime and independent result readback |
| Prediction Mission v4 | Implemented for `pipeline_smoke` and deterministic `research_trial` | Authenticated partition readmission plus task-isolated settlement, UP-execution, and DOWN-execution result receipts; no external proposal provider |

`mission dispatch submit` is the CEX operator acceptance seam. It submits one
immutable Campaign request and does not read feature rows or run search locally.
The workstation only freezes, signs, and submits identities. One cloud ACK
Job/Pod downloads the shared inputs once, admits the request, renders and
executes each round search-only, performs create-once Mission/result readback
per round, and selects the deterministic pre-holdout winner. A governed GP v4
Campaign stops there; a negative result can feed the bounded external LLM
controller, while the legacy formula lane alone retains sealed finalization.
Low-level Mission and LoopRun commands
remain diagnostics and implementation surfaces; they are not alternate evidence
paths around this contract.

## Local Data Diagnostics

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

This diagnostic command calls the registered public connector, writes a content-addressed JSONL trace and manifest, persists the full manifest as an immutable DuckDB registry revision, and writes a failure artifact on acquisition failure. It is not the production CEX admission path. Research commands reject a disk manifest that does not exactly match that revision. The acquisition path never substitutes fixtures.

## CEX Campaign

The supported CEX operator path freezes one shared input set in the cloud,
finalizes its reviewed signatures into one immutable Campaign submission, and
submits that object to the generated ACK Job:

```bash
alpha-harness mission campaign-freeze \
  --campaign-inputs /private/campaign-inputs.json \
  --input-root /mounted/materialization-run \
  --source-revision REPLACE_GIT_SHA \
  --image REPLACE_REGISTRY/research-runner@sha256:REPLACE_DIGEST \
  --campaign-root https://REPLACE_INTERNAL_OSS/research/campaigns \
  --seed 7 --seed 11 \
  --output freeze.json

# Produce signed-request.json in the separately reviewed signing step.
alpha-harness mission campaign-finalize \
  --freeze freeze.json \
  --signed-request signed-request.json \
  --attempt-id attempt-001 \
  --image REPLACE_REGISTRY/research-runner@sha256:REPLACE_DIGEST \
  --request-out campaign-request.json \
  --submission-out campaign-submission.json

alpha-harness mission campaign-id --request campaign-request.json

alpha-harness mission dispatch submit \
  --submission campaign-submission.json \
  --context monday-research-apne1 \
  --namespace monday-research
```

`campaign-execute` is the generated Job's internal entrypoint, not a manual
operator step. The older Mission and LoopRun mutation commands remain hidden,
diagnostic implementation surfaces; they cannot complete this Campaign
contract.

The Campaign commands above consume a private submission whose
request binds the exact input objects and SHA-256 values, source revision,
image digest, holdout identity, campaign-wide `declared_total_trials`, and at
least two rounds. Each round carries a unique `round_id`, unique seed, and
create-once Mission/result URLs. Signed URL query parameters are transport
only; the query-free objects are part of the Campaign identity.

The submit command creates the Job suspended, verifies its pinned execution
template, then creates the immutable Secret with that Job as its owner. It reads
the Secret back before releasing the Job and prints identities only, never the
signed URLs. The Pod rejects redirects, input/hash drift, an existing holdout
claim, and any Mission or result readback mismatch before accepting terminal
evidence. Each round records `results/mission-admission.json`, which binds the
current request SHA and the round's Mission SHA alongside the campaign and
round IDs.

One Campaign contains multiple rounds. Each round renders one Mission, executes
search only, and can produce at most one passing pre-holdout result. The
Campaign selects one deterministic pre-holdout winner from the passing rounds.
The v4 ML lane stops there. If no round passes, the Campaign ends negative and
no claim is made; its typed factor/model/replay failures can seed a bounded
follow-up. Only the legacy formula lane retains one finalization against the
global holdout claim.

The direct `mission execute` and checkpoint-resume surfaces remain diagnostic.
They are not the cloud Campaign production path.

The artifact binds one Binance Spot or USD-M instrument and horizon, typed
hypotheses and falsifiers, immutable data/policy identities, the search and
evaluation protocol, and typed prior-evidence references. The command derives
research fields, budgets, and costs from that artifact; they are not accepted
as alternate execute flags. The latest credential-free replay writer currently
admits USD-M only: public reference artifacts bind instrument rules, funding,
and open interest, while fee and rebate values remain explicit, content-hashed
Mission assumptions. Historical account-bound and Spot replay schemas remain
read-only. Unknown fields, Prediction Market fields, action requests, hash
drift, cross-instrument inputs, and same-search exposed-holdout feedback fail
before Mission admission. `operational.submitted_at` is retained for audit but
does not alter the semantic Mission identity. The fixed v4 factor plan uses 8
snapshot L2 terminals, 16 atomic plus 4 named continuous candidates, the
six-hour
protocol `7200 + 3*(3600+1) + 5 + 3600 = 21608`, and the $1000 / Top5 5%
capacity screen. A non-empty Factor Bank trains deterministic Ridge, shallow
CART, and one Burn ndarray MLP on the same purged folds. Only validation-fold predictions become cost-aware
fractional positions; training, purge, embargo, and sealed-holdout rows remain
flat. A model is selectable only after its OOS predictive/trading gates and the
same-position canonical L2 event replay both pass. The report persists the
prediction/position ledger, additive equity, turnover, drawdown, net return,
Sharpe, exact model/factor/data identities, and explicit L2 limitations. It
keeps the holdout unopened and carries neither deployment nor order-submission
authority. Governed GP v1-v3 continue to use the legacy Factor-Bank subset MCTS
and four-stage formula artifacts for historical compatibility.

The hidden diagnostic `mission run` surface requires `--feature-fields`.
Supply comma-delimited fields that are present in the prepared dataset,
live-executable, and all belong to the same live event domain. GP and LLM
produce validated Formula candidates. The low-level CLI rejects `mcts`;
legacy Factor-Bank subset MCTS is owned only by the Campaign execution seam.
`bayesian` and `offline-rl` remain research engines but are rejected before
opening mission state because their proposal grammars cannot produce
live-executable formulas. LLM diagnostics require:

```text
ALPHA_LLM_ENDPOINT
ALPHA_LLM_API_KEY
ALPHA_LLM_MODEL
ALPHA_LLM_PROVIDER  # optional
```

Credentials are process inputs and are never persisted.

For an LLM mission, `objective` and `hypothesis_scope` are the governed research brief; put the curated, source-grounded material synthesis in those fields. `prompt_snapshot_id` records the content-addressed source snapshot but is not an implicit retrieval mechanism. The proposer receives that brief, the registered feature names, the mutable scope, and at most eight prior candidate `keep`/`discard` outcomes. It receives no row labels, raw evaluation metrics, or validator thresholds, and code rejects Formula output unless `mutable_scope` explicitly includes `factor_ast` or `factor_formula`.

## Legacy Diagnostic Surfaces

`mission create`, `mission execute`, `mission run`, `mission resume`, and
`loop run` remain parseable only for focused diagnostics and legacy checkpoint
inspection. They are hidden from CLI help and are not runnable CEX completion
paths. The Campaign seam owns v4 supervised-model selection and replay, legacy
Factor-Bank subset MCTS/checkpoint resume, round accounting, and winner
selection. Only the legacy formula lane owns sealed-holdout finalization.
Missing evaluation, holdout, Paper, Shadow, or human evidence still fails or
pauses closed; invoking a diagnostic command never supplies that evidence.

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
