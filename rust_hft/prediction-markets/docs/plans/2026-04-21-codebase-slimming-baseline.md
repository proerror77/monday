# Codebase Slimming Phase 0 Baseline

Date: 2026-04-21

This is the execution baseline for
`docs/plans/2026-04-21-codebase-slimming-and-dedup-plan.md`.

## Dependency Fan-In

Commands were run locally because `cargo tree` does not compile code. Cargo
printed the existing vendored-SDK profile warning on each command.

| Surface | Evidence |
| --- | --- |
| Runner shallow tree | `cargo tree -p new-ploy-runner --edges normal --depth 2` shows `new-ploy-runner -> ploy-runner-host -> ploy-market-data`, `ploy-strategy-bundles`, `ploy-strategy-runtime`, and `sqlx`. |
| Runner tree line count | `cargo tree -p new-ploy-runner --edges normal --prefix none \| wc -l` = `1373`. |
| Runner unique package count | `cargo tree -p new-ploy-runner --edges normal --prefix none \| sort -u \| wc -l` = `533`. |
| Alloy sub-crate count in runner tree | `cargo tree -p new-ploy-runner --edges normal --prefix none \| rg '^alloy' \| wc -l` = `135`. |
| SDK fan-in | `cargo tree --workspace --edges normal -i polymarket-client-sdk` shows three runner-facing paths: `ploy-claimer`, `ploy-connectivity`, and `ploy-market-data`. |
| SQLx fan-in | `cargo tree --workspace --edges normal -i sqlx` shows `ploy-market-data`, `ploy-research`, `ploy-runner-host`, `ploy-strategy-bundles`, and `ploy-strategy-runtime`. |
| DuckDB fan-in | `cargo tree -p ploy-strategy-bundles --features parquet-feed -i duckdb` shows DuckDB only through `ploy-strategy-bundles` with `parquet-feed`. |
| Polars fan-in | `cargo tree --workspace --edges normal -i polars` shows Polars only through `ploy-research`. |
| Burn fan-in | `cargo tree --workspace --edges normal -i burn` shows Burn only through `ploy-research`. |
| Linfa fan-in | `cargo tree --workspace --edges normal -i linfa` shows Linfa directly and through `linfa-logistic` / `linfa-trees`, all under `ploy-research`. |
| Forust fan-in | `cargo tree --workspace --edges normal -i forust-ml` shows Forust only through `ploy-research`. |

## Existing Behavior Guardrails

The following existing tests cover the behaviors named in Phase 0:

| Behavior | Guardrail |
| --- | --- |
| Runtime mode dispatch | `crates/ploy-strategy-runtime/src/lib.rs::tests::roadmap_aliases_build_expected_strategy_variants` exercises runtime-level strategy construction. |
| Strategy alias resolution | `crates/ploy-strategy-bundles/src/config.rs::tests::canonical_strategy_variant_normalizes_roadmap_aliases`. |
| Live/dry-run DB failure policy | `crates/ploy-strategy-runtime/src/lib.rs::tests::treats_live_and_dry_run_db_connection_failures_as_fatal_when_configured`. |
| Replay config parsing | `crates/ploy-strategy-bundles/src/config.rs::tests::parses_replay_runtime_paths`. |
| Backtest config parsing | `crates/ploy-strategy-bundles/tests/backtest_integration.rs::toml_config_drives_backtest`. |
| PM5D entry/exit invariants | Strategy-local tests in `directional`, `three_layer`, and existing `backtest_integration` replay/backtest parity tests cover entry decisions and fill/cashflow parity. |
| Official settlement guardrail | `crates/ploy-strategy-bundles/src/feed/database.rs::tests::official_only_backtest_skips_unresolved_events`. |

## Regime Export Baseline

`ploy-research` now keeps `Regime` as one root-level export from
`ploy_operator_contracts`. `factors_new` uses the same type internally through
`FactorMeta`, but no longer exposes a second `factors_new::Regime` alias.

## Feature Matrix Smoke

Feature-matrix commands live in `scripts/check_feature_matrix.sh`.

- `scripts/check_feature_matrix.sh --quick` runs core local checks for strategy
  bundles no-default, market-data no-default, strategy-runtime no-default, and
  lean replay/backtest runner builds.
- `scripts/check_feature_matrix.sh --full` adds heavier research and runner
  checks.
- `scripts/check_feature_matrix.sh --heavy` runs checks that can compile
  DuckDB/Parquet/live SDK paths.
- `scripts/check_feature_matrix.sh --list` prints the matrix without compiling.

Heavy full validation should run in CI or an isolated target directory before
Phase 0.5 lands. Do not use live trading hosts for Rust builds.
