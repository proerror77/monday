# PLOY product workspace

PLOY is maintained inside the [Monday repository](https://github.com/proerror77/monday) at
`products/ploy`. It remains an independent Rust workspace with a TypeScript frontend so its
prediction-market product, research, agent sidecar, and operator code can evolve
without joining Monday's `rust_hft` Cargo graph.

The durable runtime, research plane, data diagnostics, training, evaluation, and
operator utilities are Rust. TypeScript is limited to `ploy-frontend`. There is
no Python compatibility lane and no nested historical workflow authority.

The authoritative prediction-research LoopRun and binary-probability model are
implemented in `crates/ploy-research`. The LoopRun owns bounded proposal evidence;
the Burn model owns only event-disjoint supervised training and out-of-sample
probability metrics. Neither owns promotion or execution authority.

The authoritative prediction-research LoopRun is implemented in Rust in
`crates/ploy-research` and exposed by the `prediction_research_loop` example.
Python helpers are compatibility or one-shot analysis tools; they do not own
LoopRun state, evidence, evaluation, promotion, or execution authority.

## Execution boundary

Monday's `rust_hft` runtime is the only production authority for risk, OMS,
reconciliation, cancellation, replacement, and order execution.

- PLOY live trading is disabled in code, not only by documentation or configuration.
- `PloyDaemon::boot` installs a gateway that rejects probe, submit, cancel, replace,
  and fill-reconciliation operations.
- The real Polymarket execution gateway is private to `ploy-connectivity` and cannot
  be injected by a production PLOY entrypoint.
- The standalone Node account-operation tools are retired. Polymarket account,
  order, cancellation, and reconciliation operations belong to `rust_hft`.
- The standard `new-ploy-runner --features full` build does not enable the legacy
  `live-execution` feature.
- The only PLOY CI authority is the root `.github/workflows/ploy-ci.yml` workflow.
- `scripts/install-platform-service.sh` is a fail-closed compatibility tombstone;
  it cannot install or enable the former standalone service.

Live configuration examples and legacy deployment assets are retained only for
historical analysis. They are not approved Monday deployment entrypoints.

## Supported development scope

The active PLOY scope inside Monday is:

- research, replay, backtesting, and evidence generation;
- prediction-market data and product contracts;
- paper-mode control-plane and operator flows;
- frontend and sidecar development;
- typed intent handoff work that preserves Monday's execution authority.

Any future attempt to reconnect PLOY to real execution requires a separate reviewed
change in Monday. That change must define the handoff into `rust_hft`, restore secrets
and protected environments from an authoritative source, and prove fail-closed risk,
OMS, reconciliation, and approval gates.

## Workspace layout

- `apps/new-ployd`: paper/control-plane daemon entrypoint
- `apps/new-ploy-runner`: replay, backtest, dry-run, and research runner
- `apps/ploy-agent-sidecar`: crash-safe, prompt-only research-agent queue worker;
  contracts that require external evidence adapters fail closed until those
  Rust adapters are bundled with the release
- `apps/ployctl`: operator client
- `apps/ploytui`: terminal operator console
- `crates/ploy-*`: PLOY domain and runtime crates
- `ploy-frontend`: operator frontend
- `contracts`: shared JSON schemas
- `config`: retained strategy and deployment examples
- `docs`: current product docs plus explicitly archived standalone material

## Local development

Run Rust commands from this directory. The workspace is pinned to Rust `1.91`.

```bash
cargo +1.91 metadata --locked --no-deps
cargo +1.91 fmt --all -- --check
cargo +1.91 test --locked -p ploy-connectivity -p ploy-daemon-host
cargo +1.91 check --locked -p new-ploy-runner --features full
```

Use package-scoped checks for ordinary development. Workspace-wide and database-backed
matrices belong to migration closeout or the root PLOY CI workflow.

The prediction snapshot must consume a typed Rust data audit; a hand-written
status is rejected:

```bash
PLOY_DATABASE__URL="${PLOY_DATABASE__URL:?required}" \
  cargo run -p ploy-market-data --features audit \
  --example market_data_gap_audit -- \
  --start-ts 2026-07-01T00:00:00Z \
  --end-ts 2026-07-02T00:00:00Z \
  --symbols BTCUSDT,SOLUSDT \
  --output /tmp/prediction-data-audit.json
```

The report separately proves Binance spot/aggTrade/L2, Chainlink, Polymarket
full depth, and official settlement coverage. Query failures are unavailable
evidence, not zero missing rows.

The full runner remains non-executing in Monday:

```bash
cargo +1.91 run -p new-ploy-runner --features full -- \
  run --config config/strategies/02-pm5d.v4-dryrun.toml --dry-run
```

Frontend checks:

```bash
npm --prefix ploy-frontend ci
npm --prefix ploy-frontend run contracts:check
npm --prefix ploy-frontend run lint
npm --prefix ploy-frontend run build
```

Rust sidecar checks:

```bash
cargo +1.91 test --locked -p ploy-agent-sidecar
cargo +1.91 clippy --locked -p ploy-agent-sidecar --all-targets --no-deps -- -D warnings
```

Do not start a local PostgreSQL service for routine migration validation. Database-backed
lanes run in the root [PLOY CI workflow](../../.github/workflows/ploy-ci.yml).

## Migration and provenance

- [Migration record](MIGRATION.md)
- [Adaptation manifest](MIGRATION_ADAPTATIONS.md)
- [Monday integration boundary](../../docs/architecture/PLOY_INTEGRATION.md)
- [Archived standalone README](docs/archive/standalone-source-2026-07-13/README.md)

The import is an adapted snapshot of PLOY
`8ce4e0f150173a44030294101f4b1371cbdf80bc`, not a claim that every tracked
blob is byte-identical at its current path. The manifest records changed, renamed,
replaced, and omitted material with hashes and reasons.
