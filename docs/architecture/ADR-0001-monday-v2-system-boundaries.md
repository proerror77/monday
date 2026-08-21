# ADR-0001: Monday V2 system boundaries and migration order

- **Status:** Accepted as an incremental architecture baseline
- **Date:** 2026-08-21
- **Scope:** Repository structure and dependency direction only. This ADR does not
  authorize a production deployment, a Gate, a cutover, or LiveSmall activation.

## Context

Monday is an end-to-end system: governed market-data acquisition, immutable data
and replay, quantitative research, strategy governance, deterministic runtime
execution, and operational evidence. The repository already documents the three
trust domains (Research, Governance, Runtime), but the current implementation
does not expose the full lifecycle as one set of module boundaries.

The current checkout provides the following evidence:

- The pre-migration Rust workspace had 77 packages. `alpha-domain` contained
  mission/evaluation types alongside strategy bundles, deployment envelopes,
  approvals, and runtime-attribution contracts. The first migration slice adds
  `hft-governance-contracts` and moves runtime-admission consumers to that
  package without changing the wire contract.
- `tools/collector` has 50 Rust source files and many recorder, reference,
  uploader, verifier, replay-materializer, cache-warmer, and analytics binaries in
  one package. Its LOB archiver also imports the runtime-oriented
  `hft-engine::binance_md` parser.
- The `alpha-harness` application owns data acquisition, mission dispatch,
  evaluation, promotion, prediction dispatch, and Kubernetes submission. It also
  links directly to `hft-collector` and `hft-backtest`, so the operator seam and
  the implementation seam are not yet the same object.
- The nested prediction-market workspace has 23 packages. `ploy-research` can
  enable a `strategy-runtime` feature that depends on `ploy-strategy-bundles` and
  `ploy-trading`; those packages contain order, execution, and reconciliation
  lifecycle code. The nested workspace is documented as transitional migration
  debt, but the dependency direction still needs to be reduced.
- A current `cargo metadata --locked --no-deps` scan found no direct dependency
  from the Monday alpha/research packages to `hft-risk` or `hft-execution`. The
  remaining coupling is through shared contract crates and transitional prediction
  features, not a reason to weaken the existing runtime boundary.

## Decision

Monday V2 is organized into five planes with explicit one-way seams:

```text
External sources
  -> Data Plane
  -> Research Control Plane
  -> Governance / Contract Plane
  -> Deterministic Runtime
  -> Operations & Evidence Plane
```

### 1. Data Plane

**Owns:** source adapters, raw append, clocks, sequence continuity, manifests,
verification receipts, canonical partitions, replay partitions, PIT materialization,
and immutable publication.

**Current roots:** `rust_hft/tools/collector`, `rust_hft/data-pipelines`,
`rust_hft/research-core/manifest`, and content-addressed trace/Parquet artifacts.

**Rules:** edge capture must remain durable and bounded; CPU/I/O-heavy validation,
compression, Parquet, ClickHouse materialization, and feature generation must be
separable release/runtime units. ClickHouse is analytics storage, not the
control-plane authority. A producer health signal never substitutes for an
independent verification receipt.

The canonical data lifecycle is:

```text
RawSegment
  -> VerificationReceipt
  -> CatalogEntry (ready | partial | rejected | superseded)
  -> CanonicalPartition
  -> ResearchSnapshot / ReplayPartition
```

### 2. Research Control Plane

**Owns:** research goals, typed missions, semantic hypotheses, search scheduling,
GP/MCTS/Bayesian/LLM proposal engines, deterministic evaluation, failure
signatures, learning directives, and research-only result artifacts.

**Current roots:** `rust_hft/alpha-harness`, `rust_hft/research-core`, and the
prediction-market research lane under `rust_hft/prediction-markets`.

LLM output is a bounded typed mission or failure critique. MCTS is one search
engine, not the workflow authority. Neither may read sealed holdout rows, submit
orders, change risk limits, resume a runtime, or load an ungoverned artifact.

### 3. Governance / Contract Plane

**Owns:** promotion records, Strategy Bundle identity, approvals, signed
deployment envelopes, runtime policy references, and signed runtime attribution.

Runtime-admission contracts now live in the small
`rust_hft/governance-contracts` crate, consumed directly by the research producer,
Store, and `apps/live`. Strategy Bundle identity and runtime attribution remain
in `alpha-domain` for later slices. The extraction is a compatibility move:
schemas, hashes, signatures, and V1 identity remain unchanged.

### 4. Deterministic Runtime

**Owns:** market connectivity, strategy execution, target-to-order planning,
pre-trade risk, OMS, portfolio truth, reconciliation, cancellation, nonce/audit
state, and execution adapters.

**Current roots:** `rust_hft/market-core`, `rust_hft/strategy-framework`,
`rust_hft/risk-control`, `rust_hft/execution-gateway`, and `rust_hft/apps/live`.

Research may emit a typed candidate and a signed bundle. Runtime remains the only
authority that can turn a validated runtime plan into an order lifecycle. The
planned strategy seam is:

```text
MarketEvent -> FeatureRuntime -> AlphaSignal -> TargetExposure
  -> PortfolioAllocator -> RiskClamp -> OrderPlanner
  -> PreTradeRisk -> OMS -> ExecutionAdapter
```

The first migration must preserve the existing `Strategy -> OrderIntent` path
behind an adapter and prove batch/replay/Paper/Shadow parity before removing it.

### 5. Operations & Evidence Plane

**Owns:** release packaging, signed artifacts, ECS/ACK/OSS/systemd controls,
formal Gates, cutover/rollback, monitoring, and independent runtime/data readback.

**Current roots:** `rust_hft/deployment`, `deployment/aliyun`, and the owning
runtime/collector operations directories.

Operations code may consume immutable artifacts and runtime status, but it does not
define research semantics or trading decisions. Code, CI, merge, release,
runtime, and readback remain separate claims.

## Dependency rules

| From | May depend on | Must not depend on |
| --- | --- | --- |
| Data Plane | market-core types, manifest/contract crates, transport/storage libraries | risk, OMS, execution adapters, LLM policy |
| Research Control | verified data artifacts, research-core, governance contracts | venue credentials, order submission, runtime resume, hard risk mutation |
| Governance / Contract | research evidence references, market/strategy identity types, signing libraries | venue clients, data collection, direct execution |
| Runtime | governance contracts, market/strategy/risk/OMS/execution modules | LLM decisions, unverified research artifacts, prediction compatibility runtime |
| Operations & Evidence | release/runtime/data identities and readback tools | candidate generation, evaluator thresholds, order construction |

Prediction-market code remains a market-family research module. Its venue adapters
and execution path migrate to the canonical Monday seams; `ploy-*` names remain
compatibility identifiers only. The `ploy-research` `strategy-runtime` feature is
not a new authority and must shrink or disappear before any live integration.

## Migration guardrails

1. Preserve V1 Mission identity and existing Paper/Shadow behavior while V2
   contracts are introduced explicitly.
2. Do not mix a contract extraction with a runtime behavior change.
3. Do not split a package merely to rename it; split only when the new release
   boundary has an owner, callers, invariants, and an independent check.
4. Every replacement keeps the old path readable until the new artifact, hash,
   Gate (when applicable), runtime state, and independent readback are recorded.
5. No change in this ADR authorizes production mutation. LiveSmall stays
   fail-closed.

## Rejected alternatives

- **Rewrite the repository:** discards proven data, governance, risk, and
  reconciliation evidence and creates an unnecessary migration cliff.
- **Make MCTS or an LLM the system controller:** gives a proposal mechanism
  authority it cannot safely hold and breaks deterministic evidence boundaries.
- **Merge prediction-market and continuous-contract evaluators:** their labels,
  split contracts, and settlement evidence differ; shared governance seams are
  sufficient.
- **Create microservices now:** process boundaries would add deployment and
  consistency cost before the ownership and artifact seams are stable.

## Consequences

This decision makes the full Monday lifecycle visible and gives each migration a
bounded owner. It also makes existing coupling explicit: the first implementation
work is contract extraction and data-plane release separation, not a new search
algorithm. The cost is temporary adapters and duplicate read-only schemas while
identity and readback are proven; those adapters must have a named removal trigger.
