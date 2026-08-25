# Prediction Markets In Monday

## Outcome

Prediction markets are a market-family module inside Monday at
`rust_hft/prediction-markets`. Polymarket, Predict.fun, Kalshi, and future
exchanges are venues, not separate product authorities. The imported `ploy-*`
crate and binary names remain temporary compatibility identifiers while their
implementations move to canonical Monday modules.

The nested Rust 1.91 Cargo workspace is a transitional build seam, not a product
or production-authority seam. It still contains imported compatibility
order/risk/reconciliation contracts for paper and test flows. Those contracts
are explicit migration debt and cannot gain a concrete venue Adapter; canonical
Monday interfaces replace them in later phases.

Monday remains the repository and production authority. In particular:

- `rust_hft` owns real market connectivity, risk, OMS, reconciliation, cancellation, and execution.
- The prediction-market module may produce research evidence, operator interactions, typed intents, and sidecar recommendations.
- Compatibility daemons and sidecars must not bypass Monday by submitting orders from legacy standalone deployment paths.
- Live trading remains disabled. This migration does not deploy, resume, approve, or mutate any trading host.

## Module ownership and seams

| Capability | Canonical Monday owner | Migration rule |
| --- | --- | --- |
| Shared instruments, orders, fills, and venue capabilities | `rust_hft/market-core` | Prediction-market types cross this interface; venue wire types stay behind an Adapter |
| Polymarket/Predict/Kalshi market data | `rust_hft/data-pipelines/adapters` | Split imported collectors by actual venue; do not grow a generic PLOY data plane |
| Event-settlement datasets, models, and evaluation | `rust_hft/prediction-markets` | Keep Brier/log-loss/calibration and settlement evidence local to this deep module |
| Strategies and typed intents | `rust_hft/strategy-framework` plus the prediction-market research module | Research emits typed intents only; runtime applies risk and policy |
| Orders, cancellation, reconciliation, and account truth | `rust_hft/execution-gateway` and `rust_hft/risk-control` | Exactly one execution path per venue; no compatibility fallback |
| ECS, ACK, OSS, and systemd assets | `deployment/aliyun` | Market-family code does not own a second deployment authority |

The external seam is deliberately small: prediction research returns evidence
and typed intents; Monday runtime returns deterministic acceptance, rejection,
and attribution. Venue-specific authentication, fee metadata, settlement rules,
and wire formats remain hidden inside their Adapters.

## Research framework boundary

Monday intentionally keeps two evaluation frameworks because the labels, sampling units, and promotion evidence are different:

| Research lane | Sampling unit and target | Primary evaluation | Owner |
| --- | --- | --- | --- |
| Derivatives / continuous contracts | Point-in-time time-series rows predicting future return | Purged walk-forward IC, RankIC, ICIR, post-cost return, turnover, and drawdown | `rust_hft/alpha-harness` |
| Prediction markets | Event observations predicting an official binary settlement | Event-disjoint walk-forward Brier score, log loss, calibration error, full-depth settlement PnL, and capacity | `rust_hft/prediction-markets/crates/ploy-research` |

The lanes share repository governance, evidence provenance requirements, and the Monday execution-authority boundary. They do not share labels, fold construction, evaluator thresholds, or a Cargo graph. In particular, IC/ICIR may be diagnostic for a prediction-market feature, but it is not a prediction-market promotion gate; prediction rows must not be routed through the derivatives `FormulaEvaluator`.

The prediction-market lane also keeps data authority explicit:

- Chainlink provides the point-in-time opening reference and the contract's
  expiry-price semantics. Polymarket's official resolved outcome, materialized
  in `pm_token_settlements`, is the binary evaluation label.
- Binance spot, aggTrade, and L2 data are external predictive and repricing
  inputs only. They must never replace the opening reference, settlement oracle,
  or Polymarket execution price.
- Polymarket CLOB quotes and full depth provide market-implied probability,
  executable entry price, fees, fillability, and capacity evidence.

### Polymarket event and model identity

The continuously-ready path first serves BTC five-minute markets. Its binding
identity hierarchy is:

| Level | Canonical identity | Contract |
| --- | --- | --- |
| Product family | `symbol x event_horizon` (`BTC x 5m` first) | One Mission and model-selection problem; other symbols or horizons are different product families. |
| Episode | `market_id` | One immutable event window and one official binary settlement. `condition_id` and market slug are audited aliases, not training keys. |
| Instrument | `token_id` | Each selected episode binds exactly one `UP` token and one `DOWN` token. Their books, trades, fees, fills, slippage, and markouts never share instrument identity. |

The settlement model emits one episode probability `p_up`
(`p_down = 1 - p_up`). The two instruments are not two settlement labels.
Token repricing, fill probability, slippage, and markout are instead independent
UP-execution and DOWN-execution tasks. They may consume the corresponding
settlement probability as a point-in-time feature, but cannot change the
official settlement label or be accepted by settlement calibration alone.

### Continuously-ready research contract

A source-closed producer segment is only a candidate. An independent typed
verifier that cannot write or repair producer evidence is the sole authority
allowed to append a catalog classification. The verifier rehashes the producer
artifacts and records its own binary, configuration, and policy identities.
Producer success, collector health, object presence, and operator inspection
cannot promote an episode to `ready`.

| State | Meaning | Selectable for a cohort |
| --- | --- | --- |
| `ready` | The episode has one exact UP/DOWN token pair and all provenance, request outcomes, clocks, sequence, completion, task-surface, and snapshot-input evidence required by the verifier policy. | Yes, only for the verified supported tasks. |
| `partial` | Authentic evidence exists but at least one required surface is incomplete or not yet closed. The verifier records reason codes and the supported subset, if any. | No. A new immutable producer revision may be verified independently; it does not rewrite this classification. |
| `rejected` | Evidence is contradictory, non-derivable, corrupt, identity-mismatched, or outside the declared product/task contract. | No. Repair requires new producer evidence and a new verification receipt; the rejected receipt remains immutable. |

Completeness is event-local. A `partial` or `rejected` sibling episode does not
block another independently complete episode in the same product family from
becoming `ready`. Conversely, evidence from a sibling must never fill the
selected episode's gap.

Catalog, snapshot, and execution handoffs authenticate identity by pinning and
rehashing immutable content, not by trusting a mutable path or status field:

| Handoff | Identity that must be bound by the next receipt |
| --- | --- |
| Producer evidence | Producer source revision, binary and configuration digests, schema identity, source-closed segment manifest, and artifact digests. |
| Verifier receipt / ready catalog entry | Producer manifest digest; verifier binary, configuration, and policy digests; classification and reason codes; exact product, episode, UP token, DOWN token, supported-task, and coverage identities. |
| Cohort manifest | Ordered ready-catalog entry digests, event-disjoint partition assignment, common-time boundary, label-availability cutoff, and causal projection rules. |
| `ResearchSnapshot` | Cohort-manifest digest plus the digests of the exact evaluator-visible bytes. It is materialized and cached before Mission admission. |
| Mission | Mission SHA over product family, typed task, run mode, authority profile, cohort partition, snapshot digest, task horizon where applicable, and all evaluator/search parameters. Raw collector paths are forbidden. |
| Runtime image | Exact repository revision, release-binary digest, and immutable OCI image digest selected by the admitted Mission. A mutable tag is not an identity. |
| Result receipt | Mission SHA, snapshot digest, image digest, lifecycle timestamps, and content digest of a create-once result bundle independently rehashed after publication. |

The research objectives remain separate even when they consume the same
authenticated snapshot:

| Typed task | Allowed label | Required evidence |
| --- | --- | --- |
| `settlement_probability` | One official binary episode outcome: `UP` or `DOWN`. | Exact market contract, causal Chainlink opening/expiry observations, official resolved outcome and availability clock, plus event-disjoint Brier score, log loss, calibration, and settlement-PnL evidence. Token markouts and fills are not settlement labels. |
| `up_execution` | UP-token fill outcome, realized execution price/slippage, or executable markout at an explicit 5, 10, 15, or 30 second horizon. | Only the bound UP token's point-in-time book/trades, request and sequence evidence, fees, latency, and declared fill/queue assumptions. Official settlement is not an execution label. |
| `down_execution` | DOWN-token fill outcome, realized execution price/slippage, or executable markout at an explicit 5, 10, 15, or 30 second horizon. | Only the bound DOWN token's point-in-time book/trades, request and sequence evidence, fees, latency, and declared fill/queue assumptions. Official settlement is not an execution label. |

`pipeline_smoke` and `research_trial` are both research-only authority profiles:

| Run mode | Authority |
| --- | --- |
| `pipeline_smoke` | May prove producer-to-result schema compatibility, admission, cache use, evaluator start, publication, and digest readback with a minimal complete cohort. It cannot emit an alpha, generalization, promotion, or profitability verdict. |
| `research_trial` | May run the typed evaluator and shared MCTS kernel only on event-disjoint train/validation/held-out cohorts. Held-out labels, metrics, and feedback cannot alter candidate search, fitting, selection, or stopping. It may publish research evidence, not activation authority. |

Neither mode authorizes Paper, Shadow, Live, artifact promotion, strategy
configuration changes, runtime enablement, or profitability claims. A Mission
requesting any such authority is rejected before a Pod or Job is created.

A `pipeline_smoke` completion uses `monday.prediction.pipeline_smoke.result.v1`
and binds the content digest of its one
`monday.prediction.pipeline_smoke.v1` evaluator report; the runner rehashes that
report after immutable result-bundle readback before accepting the completion.

The [repository work-control policy](../../AGENTS.md) is binding here: one named
write owner per branch, one independently reviewable and rollbackable change
contract per PR, and read-only review until ownership is explicitly transferred.
Prediction-market-specific mutable boundaries are also singular:

- One named runtime controller may submit, replace, delete, deploy, or otherwise
  mutate catalog and cloud runtime resources. All other agents and operators are
  read-only until an explicit handoff.
- One publisher owns create-once publication for an immutable object key. It may
  not overwrite a snapshot or result identity; independent readback rehashes the
  published bytes and does not confer branch or runtime control.

#### Binding counterexamples

| Counterexample | Required decision |
| --- | --- |
| A row claims the UP `token_id` but uses the DOWN book or trades. | Reject the episode or snapshot for token-book identity mismatch; complementary prices do not repair instrument provenance. |
| A training observation can see a Chainlink tick after its decision time, or a training event crosses the Mission's common-time boundary. | Reject the cohort or snapshot for future-reference exposure. Causal projection and the label-availability cutoff are independently required. |
| One episode is complete while an adjacent episode in the same collection segment is missing a book or request outcome. | Admit only the complete episode as `ready`; classify the adjacent episode `partial`. Never reject or repair across the sibling boundary. |
| MCTS receives held-out labels, metrics, ranking, or natural-language feedback before search is sealed. | Invalidate the `research_trial`; held-out feedback is read-only final evaluation evidence. |
| A snapshot, image tag, or result path resolves to bytes whose digest differs from the Mission or receipt, or the bytes can be overwritten in place. | Reject admission or readback. Publish new bytes under a new immutable digest; path continuity is not artifact identity. |

Chainlink reference ticks have one physical source of truth and are projected
point-in-time into each event window. To keep overlapping 5m/15m/1h events from
sharing that path across train and validation, every horizon-specific task for
an underlying must use the same mission-pinned wall-clock boundary. A training
event must end strictly before the boundary; a validation event must start at
or after it. The boundary is sealed into the immutable mission identity, binary
dataset contract, and model manifest, while the independent label-availability
cutoff remains mandatory.

The governed LoopRun forwards that mission boundary to the settlement-only
walk-forward evaluator, including the no-prior baseline turn. A missing or
invalid boundary fails closed; generic factor and token-execution reviews do
not inherit this settlement split implicitly.

The broader governed baseline remains BTC/SOL five-minute settlement research
over the retained one-second full-visible-depth L2 snapshots. The
continuously-ready catalog and Mission contract above admits only BTC
five-minute episodes; SOL remains outside this path until a separate verified
end-to-end contract admits it. Fifteen-minute and one-hour missions likewise
require their own verified end-to-end data contracts before they can claim
governed coverage. Full order-book update ticks are reserved for the later token
microstructure/execution lane; they are not a prerequisite for the settlement
baseline.

The governed snapshot keeps Binance availability semantics separate from source
time. Spot and L2 are bucket-selected by exchange time but replayed at
`received_at`; aggTrade rows are aggregated inside each five-second source-time
bucket by aggressor side, preserving buy/sell gross flow, and replayed only at
the latest contributing `received_at`. Rows with reversed clocks, excessive
source-to-arrival delay, or per-symbol source-time rollback are rejected, and
every prediction-evaluator observation must retain fresh spot, aggTrade, and L2
authority flags. At each decision timestamp, both `decision - source_ts` and
`decision - received_at` must fit the mission age bound; a fresh arrival cannot
mask an old exchange observation. The retained raw feed remains the audit
source; the aggregate is the bounded research view.

The prediction-market external research contract accepts only
`prediction_research_mission.v4`. Both `pipeline_smoke` and `research_trial`
cross the same independently authenticated cohort, partition, snapshot, policy,
task, image, publication, and readback boundary; there is no v2 mapping or
fallback. `research_trial` supports settlement probability plus side-bound UP
and DOWN execution tasks at 5, 10, 15, or 30 seconds. It re-admits the serialized
partition view before invoking the existing bounded MCTS checkpoint and typed
evaluator path. The current profile is deterministic and makes no external
proposal-provider call.

The module has its own bounded Rust prediction-research LoopRun in
`crates/ploy-research`, with `monday-prediction-research` as its precompiled
process entrypoint, because an event settlement loop cannot reuse the
derivatives return/IC state machine.
`alpha-harness prediction` owns only the shared Monday transport, evidence, and
resume envelope around that process. No compatibility script is an authoritative
prediction LoopRun or promotion surface. Standalone formula
mutations may carry a falsifiable hypothesis and compile through AutoFactor as
IC/ICIR diagnostics, but the prediction LoopRun accepts only typed
probability-blend candidates. Those candidates enter the module's
event-disjoint Brier/log-loss/calibration evaluator and return candidate-specific
deterministic feedback, including conservative-depth capacity failure. The
evaluator never invokes an LLM or accepts free-form trading instructions.

Both loops share governance rules, not evaluator code. The derivatives LoopRun
persists its state in `rust_hft/alpha-harness`; the prediction LoopRun persists
one mission-bound state plus content-addressed iteration evidence under its
output directory. Reusing an output directory with another mission, symbol, or
data snapshot fails closed. Its five-minute evaluator accepts only observations
persisted with `event_window_secs=300`; legacy observations default to zero and
must be rebuilt into a new snapshot. The mission binds the snapshot's strong
`snapshot_contract_hash`, which covers the evaluator-visible manifest semantics
and artifacts, rather than relying only on the legacy content hash.
Governed time-cohort evaluation also requires each observation's optional
`event_end_ts` to be bound to the verified market contract. Missing or
inconsistent event ends fail closed; event starts are derived from that exact
end and the governed window instead of integer `time_remaining_secs`.

Prediction snapshot v2 atomically reads the exact UP/DOWN token-primary-key
pair, its complementary official outcome, and the local availability time of
both persisted token versions from `pm_token_settlements`. It validates shared,
non-conflicting market identities without assuming a numeric event ID equals a
human-readable market slug. Coverage gates reject
missing or impossible clocks, while the loader hashes and parses the same
captured artifact bytes. Burn training accepts only a sealed snapshot reloaded
against the mission's trusted `snapshot_contract_hash`. The shared walk-forward
path used by the actual LoopRun also requires training-label availability—not
scheduled event expiry—to precede the first retained validation decision.
Existing v1 snapshots therefore require a deterministic rebuild from retained
raw data; they are never silently upgraded.

The active Mission v4 path retains bounded MCTS checkpoints and immutable result
receipts but has no HTTP proposal client, LLM credential, provider/model usage
ledger, or checked-in v2 mission template. Neither research loop owns execution
or live activation.

## Source and provenance

- Source repository: `https://github.com/proerror77/ploy`
- Source branch: `main`
- Source SHA: `8ce4e0f150173a44030294101f4b1371cbdf80bc`
- Source commit date: `2026-07-13T21:34:10+08:00`
- Source commit: `fix: harden Polymarket tick-level live hot path (#755)`
- Import mode: adapted tracked source snapshot, not full Git history
- Declared license: MIT in the upstream Cargo metadata and README
- License caveat: the upstream root had no project-level `LICENSE` file at the source SHA

The source checkout was shallow, so the provenance baseline was produced with
`git archive` at the source SHA. The Monday tree is an adapted snapshot: security
hardening, repository metadata, execution-authority enforcement, conflict cleanup,
and formatting normalization changed selected tracked blobs. Ignored local data,
targets, node modules, virtual environments, and runtime logs were not copied.
The complete path and SHA-256 record is in
`rust_hft/prediction-markets/MIGRATION_ADAPTATIONS.md`.

## Deliberate exclusions and replacements

- `.env.production` was imported with empty credential values, renamed, and then
  moved into `docs/archive/standalone-operations` with the legacy deployment and
  infrastructure assets.
- Standalone agent/session state (`.claude`, `.superpowers`, `.full-review`, and the old `CLAUDE.md`) was excluded.
- The 1.48 MB standalone `tasks/todo.md` session log was replaced with a concise Monday migration tracker.
- The unused vendored Polymarket SDK was excluded; the workspace resolves the maintained crates.io dependency and the vendor directory was not a workspace member.
- The module-local `AGENTS.md` was rewritten for the Monday authority boundary.
- The standalone README is preserved under
  `rust_hft/prediction-markets/docs/archive/standalone-source-2026-07-13`; the active README
  documents Monday-only development and execution boundaries.
- The write-capable standalone `ploy-openclaw` bytes were preserved in that
  archive; its upstream instruction file was renamed to `AGENTS.upstream.md` and
  replaced at the active name by an archive-only safety guard. The only active
  OpenClaw example is read-only and rejects unlisted RPC methods and remote-control
  mutations before SSH.

The imported nested `.github` workflow tree was retired during the Rust-only
cutover. The former `deployment` and `infra` trees now live under
`docs/archive/standalone-operations`; they are historical evidence, not Monday
deployment entrypoints.

## Language boundary

- Durable Monday market data, account, order, reconciliation, sidecar, report,
  backtest, and monitoring paths are Rust. TypeScript is limited to the operator
  frontend.
- Training and inference use native Rust libraries. Continuous-contract models
  use the `hft-research-ml` Burn trainer and immutable Burnpack bundles;
  prediction-market probability models use the module's event-disjoint Burn trainer.
  PyTorch/libtorch bindings and tracked Python source are forbidden by root CI.
- Shell remains for host bootstrap, CI command composition, and package installation;
  no shell script owns trading decisions, risk, OMS, or exchange mutations.
- The Rust sidecar is built and tested but has no approved deployment package. Its
  missing evidence adapters fail closed and require a separate parity and deployment
  review rather than falling back to ungoverned tools.

## Historical local-only documents

Seven documents from local PLOY commit `5de411bbe8889284b47fe9932821af077d2962fc` are preserved under `rust_hft/prediction-markets/docs/archive/local-readiness-2026-07-11`. They are explicitly stale: they target the standalone PLOY/Tango topology and must not be treated as current Monday plans.

## Migration sequence and status

The migration is intentionally incremental so every slice keeps one working,
fail-closed system instead of attempting a high-risk rewrite of the imported
workspace.

| Phase | Status | Exit condition |
| --- | --- | --- |
| 0. Establish ownership | Complete | Source lives at `rust_hft/prediction-markets`; `products/ploy` is absent; CI enforces the layout |
| 1. Remove credentialed compatibility Adapters | Complete | Compatibility code contains no private-key handling, authenticated client construction, or concrete Polymarket gateway; production boot remains fail closed |
| 2. Split venue data Adapters | Planned | Polymarket, Predict.fun, Binance, and Deribit collectors implement canonical `data-pipelines` interfaces; superseded compatibility copies are deleted |
| 3. Promote shared runtime contracts | Planned | Reusable instruments, fees, typed intents, risk, OMS, monitoring, execution contracts, and reconciliation depend on canonical Monday interfaces rather than `ploy-*` compatibility types |
| 4. Retire the compatibility workspace | Planned | Entrypoints and operator UI compose Monday modules, package names are functional, and the nested Cargo workspace can be removed |

Each later phase must add contract tests at the receiving seam before moving an
implementation. A phase is not complete while both the compatibility copy and
the canonical implementation remain active.

## CI and maintenance

- Monday's existing Rust workspace stays rooted at `rust_hft` and keeps its own toolchain and CI.
- The transitional prediction-market workspace stays rooted at `rust_hft/prediction-markets`, uses Rust `1.91` and Node `22`, and has a dedicated root workflow at `.github/workflows/ploy-ci.yml`.
- Prediction-market-only changes do not run Monday's main Rust or Docker build matrices; repository-wide security checks still scan the full diff.
- Root workflows and active source remain scanned, and tracked-secret detection
  covers the complete repository tree. There is no nested prediction-market workflow surface.
- The active compatibility entrypoints are `new-ployd`, `ploy-agent-sidecar`, `new-ploy-runner`, `ployctl`, and `ploytui`. The root `ploy` crate is a compatibility shim; new entrypoints use functional Monday names.
- `PloyDaemon::boot` installs `DisabledLiveExecutionGateway`; production code
  cannot inject a concrete venue Adapter. The former private Polymarket gateway
  and its SDK/private-key dependencies have been removed from the compatibility
  module, and the standard runner `full` feature does not enable its legacy
  control-plane live executor.
- The compatibility `LiveExecutionGateway` contract and paper/runtime state are
  not canonical Monday interfaces. They remain migration debt until Phase 3 and
  cannot be implemented by a credentialed venue client in this module.
- The standalone Node account-operation packages are retired. Polymarket account,
  order, cancellation, and reconciliation operations remain in canonical Monday modules; the prediction-market module
  does not retain a second execution path.
- The standalone SDK authentication probe is retired. Prediction-market tooling
  does not read venue private keys or construct authenticated account clients.

## Operations and archive boundary

The former PLOY repository had 20 repository secrets, 4 variables, 8 environments, protected live approvals, scheduled workflows, 28 open issues, and standalone deployment paths. Secret values cannot be read back and were intentionally not copied because the legacy workflows are not activated in Monday.

Before any future prediction-market deployment is enabled, create a separate reviewed change that rebuilds the required secret sources, environments, branch protection, immutable artifacts, runner identity, and host-path mapping inside Monday. That work must keep Monday's execution authority and fail-closed live gates intact.

The archived issue index remains available at `https://github.com/proerror77/ploy/issues?q=is%3Aissue%20is%3Aopen`; issues `#751` and `#361` are the most recent infrastructure follow-ups and are historical blockers, not active deployment approval.

## Acceptance criteria

- The adapted PLOY source snapshot and local-only design documents are preserved with explicit provenance.
- The prediction-market module builds and tests from `rust_hft/prediction-markets`; its nested Cargo graph remains a transitional build seam.
- The retired `products/ploy` path is absent and guarded by CI.
- Frontend contract checks and Rust sidecar tests run from the Monday repository.
- Repository secret scanning passes without expanding the allowlist.
- No legacy standalone workflow is activated at the Monday workflow root.
- The former PLOY repository is redirected to Monday and archived only after the Monday migration PR is merged and green.
