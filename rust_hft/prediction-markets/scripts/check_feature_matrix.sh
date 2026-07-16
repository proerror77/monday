#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

target_dir="${CARGO_TARGET_DIR:-/tmp/ploy-feature-matrix}"
export CARGO_TARGET_DIR="$target_dir"

quick_commands=(
  "cargo check -p ploy-strategy-bundles --no-default-features --lib"
  "cargo check -p ploy-market-data --no-default-features --lib"
  "cargo check -p ploy-strategy-runtime --no-default-features --lib"
  "cargo check -p ploy-replay"
  "cargo check -p ploy-backtest"
)

full_commands=(
  "cargo check -p new-ploy-runner"
  "cargo check -p ploy-research --lib"
)

heavy_commands=(
  "cargo check -p ploy-strategy-bundles --features parquet-feed --lib"
  "cargo check -p ploy-market-data --lib"
  "cargo check -p ploy-strategy-runtime --lib"
)

run_step() {
  printf '\n==> %s\n' "$*"
  "$@"
}

run_quick() {
  run_step cargo check -p ploy-strategy-bundles --no-default-features --lib
  run_step cargo check -p ploy-market-data --no-default-features --lib
  run_step cargo check -p ploy-strategy-runtime --no-default-features --lib
  run_step cargo check -p ploy-replay
  run_step cargo check -p ploy-backtest
}

run_full_only() {
  run_step cargo check -p new-ploy-runner
  run_step cargo check -p ploy-research --lib
}

run_heavy() {
  run_step cargo check -p ploy-strategy-bundles --features parquet-feed --lib
  run_step cargo check -p ploy-market-data --lib
  run_step cargo check -p ploy-strategy-runtime --lib
}

usage() {
  cat <<'USAGE'
Usage: scripts/check_feature_matrix.sh [--quick|--full|--heavy|--list]

Runs the feature/build smoke matrix used by the codebase-slimming plan.

Options:
  --quick   Run local-safe lean/no-default and mode-binary checks.
  --full    Run quick checks plus full runner/research checks.
  --heavy   Run checks that can compile DuckDB/Parquet/live SDK paths.
  --list    Print the command matrix without executing it.

Environment:
  CARGO_TARGET_DIR  Defaults to /tmp/ploy-feature-matrix.
USAGE
}

mode="${1:---quick}"

case "$mode" in
  --quick)
    printf 'Using CARGO_TARGET_DIR=%s\n' "$CARGO_TARGET_DIR"
    run_quick
    ;;
  --full)
    printf 'Using CARGO_TARGET_DIR=%s\n' "$CARGO_TARGET_DIR"
    run_quick
    run_full_only
    ;;
  --heavy)
    printf 'Using CARGO_TARGET_DIR=%s\n' "$CARGO_TARGET_DIR"
    run_heavy
    ;;
  --list)
    printf 'Quick matrix:\n'
    printf '  %s\n' "${quick_commands[@]}"
    printf 'Full-only matrix:\n'
    printf '  %s\n' "${full_commands[@]}"
    printf 'Heavy matrix:\n'
    printf '  %s\n' "${heavy_commands[@]}"
    exit 0
    ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
