#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
"$root/rust_hft/scripts/workspace-metadata.sh" >"$work/metadata.json"
jq -e '[.packages[].name] | length == (unique | length)' "$work/metadata.json" >/dev/null
MONDAY_CARGO_DRY_RUN=1 "$root/rust_hft/scripts/cargo-scoped.sh" check \
  -p hft-data -p hft-research-platform -p hft-live --locked >"$work/plan.jsonl"
jq -se 'length==3 and all(.[]; index("--locked")!=null) and
  ([.[] | .[3]] | unique | length)==3' "$work/plan.jsonl" >/dev/null
if "$root/rust_hft/scripts/cargo-scoped.sh" check -p missing-package --locked >"$work/rejected" 2>&1; then
  echo 'unknown owner was admitted' >&2; exit 1
fi
if "$root/rust_hft/scripts/cargo-scoped.sh" check -p hft-data -p hft-live --features json-simd >"$work/rejected" 2>&1; then
  echo 'ambiguous cross-workspace feature matrix was admitted' >&2; exit 1
fi
cargo tree --manifest-path "$root/rust_hft/research-core/platform/Cargo.toml" \
  -p hft-research-platform --features control --locked --edges normal --prefix none >"$work/control.tree"
if grep -E '^(burn|ort |hft-(collector|research-ml|data) |parquet )' "$work/control.tree"; then
  echo 'control pulls acquisition, training or Parquet' >&2; exit 1
fi
cargo tree --manifest-path "$root/rust_hft/data-pipelines/Cargo.toml" \
  -p hft-data --locked --edges normal --prefix none >"$work/data.tree"
if grep -E '^(burn|ort |hft-(collector|research-ml|execution-adapter-[a-z-]+) )' "$work/data.tree"; then
  echo 'data protocol library pulls acquisition, training or execution' >&2; exit 1
fi
for feature in default import; do
  options=()
  [[ $feature == default ]] || options=(--features import)
  cargo tree --manifest-path "$root/rust_hft/data-pipelines/Cargo.toml" \
    -p hft-market-pipeline "${options[@]}" --locked --edges normal --prefix none >"$work/pipeline.tree"
  if grep -E '^(burn|ort |hft-(collector|research-platform|research-ml|execution-adapter-[a-z-]+) )' "$work/pipeline.tree"; then
    echo 'conversion/import pulls acquisition, scheduling, training or execution' >&2; exit 1
  fi
done
printf 'workspace ownership, scoped commands, feature rejection and thin closures passed\n'
