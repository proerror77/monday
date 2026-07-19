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

`polymarket-raw-ops collect-holder-flow` adds Polygon CTF holder flow as a research-only
input from contiguous confirmed blocks. Its output is range flow,
not an absolute balance unless collection starts before the token's first
transfer. It may enter a prediction `ResearchSnapshot`; it must not feed
execution or bypass the CLOB, risk, OMS, and reconciliation path.

LLM proposal paths remain lane-specific as well. `alpha-harness` has a bounded,
lab-only Formula proposer for derivatives missions. The prediction-market module uses the versioned
`prediction_research_mission.v1` JSON brief and its existing `LlmPriorSpec`; it
does not import the alpha-harness Rust domain or loop runtime. Instead, the module has
its own bounded Rust prediction-research LoopRun in `crates/ploy-research`, with
`monday-prediction-research` as its precompiled process entrypoint, because an
event settlement loop cannot reuse the derivatives return/IC state machine.
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

The prediction LoopRun also keeps an append-only ledger for every LLM call and
links each accepted proposal to its full retry lineage. Prompts, responses,
priors, evaluator attempts, feedback, and deterministic decisions are
content-addressed. A persisted response can be replayed after a process crash
without another provider call; missing responses and rejected retries remain
explicit evidence rather than disappearing from the budget. Neither loop owns
execution or live activation.

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
- The write-capable standalone `ploy-openclaw` package was relocated byte-for-byte
  into that archive. The only active OpenClaw example is read-only and rejects
  unlisted RPC methods and remote-control mutations before SSH.

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
