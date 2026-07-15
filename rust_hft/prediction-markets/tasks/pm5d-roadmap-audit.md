# PM5D roadmap audit against the refactored runtime

Date: 2026-04-11

Sources of truth:
- `tasks/strategy-evolution-plan.md`
- `.omx/context/pm5d-strategy-implementation-20260411T062312Z.md`

## Coverage matrix

| Roadmap version | Current status | Evidence |
| --- | --- | --- |
| V1 — baseline | Implemented as `strategy_variant = "v1"` aliasing to the directional engine with broader baseline params and V2/V3 enhancements disabled in config. | `crates/ploy-strategy-runtime/src/lib.rs`, `config/strategies/02-pm5d.v1-{dryrun,live}.toml` |
| V2 — tightened | Implemented as `strategy_variant = "v2"` aliasing to the directional engine with tightened entry params and V2/V3 enhancements disabled in config. | `crates/ploy-strategy-runtime/src/lib.rs`, `config/strategies/02-pm5d.v2-{dryrun,live}.toml` |
| V3 — multi-vol + price structure | Implemented on the directional engine with `ReturnBuffer`, realized/Parkinson volatility selection, and odds-ratio price-structure adjustment enabled in config. | `crates/ploy-strategy-bundles/src/strategies/directional.rs`, `config/strategies/02-pm5d.v3-{dryrun,live}.toml` |
| V4 — mean reversion | Implemented as `strategy_variant = "v4"`/`"mean_reversion"` with a standalone `MeanReversionStrategy` prototype using spot + PM quote + return-buffer reversal logic and early exits. | `crates/ploy-strategy-bundles/src/strategies/mean_reversion.rs`, `config/strategies/02-pm5d.v4-{dryrun,live}.toml` |

## Live/runtime gaps that still remain

1. Historical/replay support already includes `MarketUpdate::L2` from `binance_lob_ticks`.
2. Live/dry-run runtime wiring still does **not** inject Binance L2 into strategy runtime.
3. Current V4 intentionally stays off LOB so it remains runnable today.
4. Full V3 LOB confirmation and V4+LOB still require live Binance L2 ingestion plus upstream LOB collector stability work.

## Actionable conclusion

- You can compare `v1`, `v2`, `v3`, and `v4` now with explicit runtime/config surfaces.
- `v4` is a prototype, not the final LOB-aware version.
- The next infra step is still: live L2 ingestion + collector stabilization.
