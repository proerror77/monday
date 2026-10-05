#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
"$root/rust_hft/scripts/workspace-metadata.sh" >"$work/metadata.json"
jq -e '[.packages[].name] | length == (unique | length)' "$work/metadata.json" >/dev/null
MONDAY_CARGO_DRY_RUN=1 "$root/rust_hft/scripts/cargo-scoped.sh" check \
  -p hft-data -p hft-cex-research-input -p hft-research-platform -p hft-live --locked >"$work/plan.jsonl"
jq -se 'length==4 and all(.[]; index("--locked")!=null) and
  ([.[] | .[3]] | unique | length)==4' "$work/plan.jsonl" >/dev/null
if "$root/rust_hft/scripts/cargo-scoped.sh" check -p missing-package --locked >"$work/rejected" 2>&1; then
  echo 'unknown owner was admitted' >&2; exit 1
fi
if "$root/rust_hft/scripts/cargo-scoped.sh" check -p hft-data -p hft-live --features json-simd >"$work/rejected" 2>&1; then
  echo 'ambiguous cross-workspace feature matrix was admitted' >&2; exit 1
fi
cargo tree --manifest-path "$root/rust_hft/shared/Cargo.toml" \
  -p hft-cex-research-input --locked --edges normal --prefix none >"$work/input.tree"
if grep -E '^(burn|tract-|ort |sqlx-|axum |reqwest |hft-(collector|research-platform|research-ml|execution-adapter-[a-z-]+) )' "$work/input.tree"; then
  echo 'scientific input pulls database, transport, control, training or execution' >&2; exit 1
fi
cargo tree --manifest-path "$root/rust_hft/research-core/Cargo.toml" \
  -p hft-backtest --locked --edges normal --prefix none >"$work/backtest.tree"
if grep -E '^hft-research-platform ' "$work/backtest.tree"; then
  echo 'backtest still depends on the control platform' >&2; exit 1
fi
for profile in default db full; do
  options=()
  [[ $profile == default ]] || options=(--features db)
  cargo tree --manifest-path "$root/rust_hft/prediction-markets/Cargo.toml" \
    -p ploy-research "${options[@]}" --locked --edges normal --prefix none >"$work/prediction.tree"
  if grep -E '^(alpha-(domain|engine|harness|onnx-evaluator) |hft-(cex-research-input|collector|backtest|research-platform) )' "$work/prediction.tree"; then
    echo 'prediction research pulls the CEX input, evaluator or control chain' >&2; exit 1
  fi
done
cargo tree --manifest-path "$root/rust_hft/research-core/platform/Cargo.toml" \
  -p hft-research-platform --features control,gateway --locked --edges normal --prefix none >"$work/control.tree"
if grep -E '^(burn|ort |hft-(collector|research-ml|data) |parquet )' "$work/control.tree"; then
  echo 'control pulls acquisition, training or Parquet' >&2; exit 1
fi
# Depth acquisition and book sequencing must not compile the order-loop engine.
for selection in 'data-pipelines/Cargo.toml hft-collector' 'data-pipelines/Cargo.toml hft-binance-md' 'runtime/Cargo.toml hft-binance-depth'; do
  read -r manifest package <<<"$selection"
  cargo tree --manifest-path "$root/rust_hft/$manifest" -p "$package" \
    --locked --edges normal --prefix none >"$work/depth.tree"
  if grep -E '^(hft-engine |alpha-(domain|engine|onnx-evaluator|harness) |burn |ort |hft-(research-ml|research-platform|execution-adapter-[a-z-]+) )' "$work/depth.tree"; then
    echo 'depth acquisition imports runtime engine, science or execution authority' >&2; exit 1
  fi
done
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
for package in hft-research-artifacts hft-prediction-research-worker; do
  if [[ $package == hft-research-artifacts ]]; then owner=shared; else owner=prediction-markets; fi
  cargo tree --manifest-path "$root/rust_hft/$owner/Cargo.toml" \
    -p "$package" --locked --edges normal --prefix none >"$work/worker.tree"
  if grep -E '^(alpha-(domain|engine|harness|onnx-evaluator) |burn |ort |hft-(collector|backtest|research-platform|research-ml|execution-adapter-[a-z-]+) )' "$work/worker.tree"; then
    echo 'Prediction/artifact worker imports acquisition, CEX research, control, training or execution' >&2; exit 1
  fi
done
printf 'workspace ownership, scoped commands, feature rejection and thin closures passed\n'
