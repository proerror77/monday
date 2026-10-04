# Monday Repository Layout

## Rule

Monday is one multi-venue trading system. Directories follow the module that owns
an interface, not the source repository, exchange brand, deployment host, or
temporary project name that first introduced the implementation.

## Canonical roots

| Path | Owns | Must not own |
| --- | --- | --- |
| `rust_hft/market-core` | Shared market, instrument, order, fill, and runtime interfaces | Venue authentication or wire formats |
| `rust_hft/data-pipelines` | Acquisition, normalization, replay, and venue market-data Adapters | Orders or account mutation |
| `rust_hft/prediction-markets` | Event-settlement research, probability evaluation, replay, and operator tooling | A second risk, OMS, reconciliation, or execution stack |
| `rust_hft/strategy-framework` | Deterministic strategies and typed intent production | Direct venue calls |
| `rust_hft/risk-control` | Risk, OMS, portfolio truth, and reconciliation policy | LLM or research decisions |
| `rust_hft/execution-gateway` | Venue execution interfaces and concrete Adapters | Research evaluation |
| `rust_hft/apps` | Runtime composition and operator entrypoints | Duplicated domain implementations |
| `rust_hft/deployment` | Venue-neutral runtime images, manifests, and release packaging | Provider-specific host inventory or cloud credentials |
| `deployment/aliyun` | ECS, ACK, OSS, systemd, release, and health-control assets | Trading decisions or research models |
| `docs/architecture` | Current architecture and ownership contracts | Generated reports or abandoned plans |
| `docs/reports` | Dated evaluation and operational evidence | Canonical interfaces |

`products/ploy` is retired. PLOY remains only as a compatibility name for
imported crates, binaries, and provenance records while capabilities migrate to
their canonical Monday modules.

## Placement decision

Before creating a directory, answer these questions in order:

1. Does it implement an existing market-data or execution interface for one
   exchange? Put it beside the other Adapters at that seam.
2. Is it reusable market, order, risk, or strategy behavior? Put it in the
   canonical core module that owns that interface.
3. Is it specific to event settlement, probability calibration, or prediction
   research? Put it in `rust_hft/prediction-markets`.
4. Is it a cloud/runtime asset? Put it under `deployment/aliyun` or the owning
   runtime's `deployment` directory.
5. Is it generated, cached, downloaded, or local agent state? Ignore it; do not
   create a new tracked root.

Do not create root-level `common`, `shared`, `utils`, `misc`, `new`, or
exchange-branded product trees. A new root requires an architecture change that
names its interface, callers, invariants, failure modes, and owner.

## Cargo build boundaries

`rust_hft/workspaces.json` registers six functional Cargo workspaces. Each owns a
lockfile and resolves features independently with Rust 1.98.1 and resolver 2.
Source paths remain stable. Each package declares exactly one workspace owner.

| Workspace manifest below `rust_hft` | Responsibility |
| --- | --- |
| `shared/Cargo.toml` | Shared market, governance, configuration, and research contracts |
| `data-pipelines/Cargo.toml` | Protocols, adapters, acquisition tools, and the independent market conversion/import pipeline |
| `research-core/Cargo.toml` | Scientific search, training, backtest, and Alpha harness |
| `research-core/platform/Cargo.toml` | Thin PG/CH research control and compute lifecycle |
| `runtime/Cargo.toml` | Live composition, strategy runtime, risk, OMS, execution, and runtime infrastructure |
| `prediction-markets/Cargo.toml` | Existing event-settlement research and operator tooling |

Use the owning manifest for feature matrices. `scripts/cargo-scoped.sh` routes
explicit package lists to their owners. No default command builds all workspaces.
Cross-domain path dependencies and their contract tests remain explicit.

The shared owner includes `hft-cex-research-input`: immutable CEX DataView contracts,
bounded binary decoding, and verified batch reuse. Backtest and control consume
this crate directly. SQL plans and HTTPS acquisition remain in the control
implementation; the input crate has no database or provider dependency.
Prediction event/settlement inputs remain in their prediction owner. A shared
Cargo owner does not make CEX time-horizon labels valid for event settlement.

The build boundary does not grant product or execution authority. Existing
`ploy-*` names remain compatibility identifiers. New packages use functional
Monday names. Legacy prediction risk and execution contracts remain migration
debt; they cannot gain another concrete venue adapter.

## Enforced invariants

- `products/ploy` must not exist.
- Monday owns both Polymarket market-data and execution Adapters.
- Prediction research has no direct order, wallet, cancel, or reconciliation path.
- Only `rust_hft/risk-control` and `rust_hft/execution-gateway` own live account mutation.
- Legacy prediction-market deployment and infrastructure trees remain historical
  until a separately reviewed migration moves an asset into active Monday operations.
