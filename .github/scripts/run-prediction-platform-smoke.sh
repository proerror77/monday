#!/usr/bin/env bash
# The platform smoke exercises in-memory state and a local child process.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../../rust_hft/prediction-markets"

SECONDS=0
cargo test --locked -p ploy --test platform_smoke
if [[ -n ${GITHUB_STEP_SUMMARY:-} ]]; then
  printf '### Rust integration regressions lane\nElapsed seconds: %s\n' "$SECONDS" >>"$GITHUB_STEP_SUMMARY"
fi
