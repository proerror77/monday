#!/usr/bin/env bash
# Run market-data contracts against this job's migrated PostgreSQL service.
set -euo pipefail
SECONDS=0
cd "$(dirname "$0")/../../rust_hft/prediction-markets"
: "${DATABASE_URL:?ephemeral PostgreSQL URL required}"
: "${PLOY_TEST_DATABASE_URL:?ephemeral PostgreSQL test URL required}"
: "${SQLX_CLI_VERSION:?pinned SQLx CLI version required}"
[[ $(sqlx --version) == "sqlx-cli $SQLX_CLI_VERSION" ]] || { echo 'pinned SQLx CLI is not prepared' >&2; exit 2; }

cargo check --locked -p ploy-market-data --features live --lib
cargo test --locked -p ploy-market-data --features live --lib

sqlx migrate run

# Reuse the live lib test above. Missing or renamed tests must fail this lane.
pg_test=predict_fun::tests::postgres_predict_fun_restart_restores_clock_and_persists_failure_readiness
cargo test --locked -p ploy-market-data --features live --lib -- --ignored --list \
  | grep -Fx "$pg_test: test"
cargo test --locked -p ploy-market-data --features live --lib "$pg_test" -- --ignored --exact

echo "### Rust market-data ops lane" >> "$GITHUB_STEP_SUMMARY"
echo "Elapsed seconds: ${SECONDS}" >> "$GITHUB_STEP_SUMMARY"
