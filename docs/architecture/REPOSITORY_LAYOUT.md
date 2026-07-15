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

## Transitional build seam

`rust_hft/prediction-markets` temporarily keeps its own Cargo workspace and Rust
toolchain so the imported code remains independently verifiable during migration.
That build separation does not grant product or execution authority. Existing
`ploy-*` package names are compatibility identifiers; new packages use functional
Monday names, and every migrated implementation deletes its superseded copy.
Legacy order/risk/reconciliation contracts inside that workspace are explicit
migration debt: they may only shrink and cannot gain a concrete venue Adapter.

## Enforced invariants

- `products/ploy` must not exist.
- Monday owns both Polymarket market-data and execution Adapters.
- Prediction research has no direct order, wallet, cancel, or reconciliation path.
- Only `rust_hft/risk-control` and `rust_hft/execution-gateway` own live account mutation.
- Legacy prediction-market deployment and infrastructure trees remain historical
  until a separately reviewed migration moves an asset into active Monday operations.
