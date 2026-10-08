#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# This mode validates only the two independently built CEX products.
mode=all
if (($#)); then
  if [[ $# != 1 || $1 != --cex-products ]]; then
    echo 'usage: test-rust-workspaces.sh [--cex-products]' >&2
    exit 2
  fi
  mode=cex-products
fi

cex_graph_failure() {
  local assertion=$1 graph=$2
  printf 'CEX dependency assertion failed: %s\n' "$assertion" >&2
  printf 'Actual related package features from %s:\n' "${graph##*/}" >&2
  awk -F'|' '
    $1 ~ /^(alpha-(harness|engine|onnx-evaluator)|hft-(cex-research-worker|research-ml|infer-onnx)|burn[^ ]*|ort) / {
      print; found=1
    }
    END {if (!found) print "<no related package rows>"}
  ' "$graph" | LC_ALL=C sort -u >&2
  exit 1
}

check_cex_feature() {
  local product=$1 graph=$2 package=$3 feature=$4 state=$5
  # The final delimiter keeps Cargo's duplicate marker outside the feature list.
  if ! awk -F'|' -v package="$package" -v feature="$feature" -v state="$state" '
    $1 ~ ("^" package " ") {
      found=1
      enabled=index("," $2 ",", "," feature ",") > 0
      if (NF != 3 || enabled != (state == "enabled")) invalid=1
    }
    END {exit (!found || invalid)}
  ' "$graph"; then
    cex_graph_failure "$product $package/$feature must be $state" "$graph"
  fi
}

check_cex_products() {
  # {f} includes active package features forwarded by the selected product.
  # Separate roots preserve the actual operator and worker feature selections.
  cargo tree --manifest-path "$root/rust_hft/research-core/Cargo.toml" \
    -p alpha-harness --locked --edges normal --prefix none --color never \
    --format '{p}|{f}|' >"$work/cex-operator.tree"
  if grep -Eq '^(burn|ort |alpha-onnx-evaluator |hft-(infer-onnx|research-ml) )' "$work/cex-operator.tree"; then
    cex_graph_failure 'operator must exclude training and ONNX implementations' "$work/cex-operator.tree"
  fi
  check_cex_feature operator "$work/cex-operator.tree" alpha-engine llm enabled

  cargo tree --manifest-path "$root/rust_hft/research-core/Cargo.toml" \
    -p hft-cex-research-worker --locked --edges normal --prefix none --color never \
    --format '{p}|{f}|' >"$work/cex-worker.tree"
  if ! grep -q '^hft-research-ml ' "$work/cex-worker.tree"; then
    cex_graph_failure 'worker must include hft-research-ml' "$work/cex-worker.tree"
  fi
  if ! grep -q '^burn ' "$work/cex-worker.tree"; then
    cex_graph_failure 'worker must include Burn' "$work/cex-worker.tree"
  fi
  check_cex_feature worker "$work/cex-worker.tree" alpha-harness scientific enabled
  check_cex_feature worker "$work/cex-worker.tree" alpha-engine kernel enabled
  check_cex_feature worker "$work/cex-worker.tree" alpha-engine fitting enabled
  check_cex_feature worker "$work/cex-worker.tree" alpha-harness default disabled
  check_cex_feature worker "$work/cex-worker.tree" alpha-harness operator disabled
  check_cex_feature worker "$work/cex-worker.tree" alpha-engine llm disabled
  printf 'CEX operator and scientific worker dependency contracts passed\n'
}

if [[ $mode == cex-products ]]; then
  check_cex_products
  exit 0
fi

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
# CEX control verifies immutable evidence without compiling a fitter or ONNX.
cargo tree --manifest-path "$root/rust_hft/research-core/Cargo.toml" \
  -p alpha-engine --no-default-features --locked --edges normal --prefix none >"$work/cex-control.tree"
if grep -E '^(burn|ort |alpha-onnx-evaluator |hft-(infer-onnx|research-ml) )' "$work/cex-control.tree"; then
  echo 'CEX evidence verification pulls a training or ONNX implementation' >&2; exit 1
fi
cargo tree --manifest-path "$root/rust_hft/shared/Cargo.toml" \
  -p hft-cex-research-input --features streaming --locked --edges normal --prefix none >"$work/cex-input.tree"
if grep -E '^(burn|ort |alpha-onnx-evaluator |hft-(infer-onnx|research-ml) )' "$work/cex-input.tree"; then
  echo 'Immutable CEX input readers pull a scientific implementation' >&2; exit 1
fi
check_cex_products
for profile in default db full; do
  options=()
  [[ $profile == default ]] || options=(--features "$profile")
  cargo tree --manifest-path "$root/rust_hft/prediction-markets/Cargo.toml" \
    -p ploy-research "${options[@]}" --locked --edges normal --prefix none >"$work/prediction.tree"
  if grep -E '^(alpha-(domain|engine|harness|onnx-evaluator) |hft-(cex-research-input|collector|backtest|research-platform) )' "$work/prediction.tree"; then
    echo 'prediction research pulls the CEX input, evaluator or control chain' >&2; exit 1
  fi
  # Prove the full graph includes science and export, beyond the db-only graph.
  if [[ $profile == full ]] &&
    { ! grep -q '^burn ' "$work/prediction.tree" || ! grep -q '^polars ' "$work/prediction.tree"; }; then
    echo 'prediction full graph is missing training or columnar export dependencies' >&2; exit 1
  fi
done
for profile in default researcher-verification control,gateway,publisher,researcher-verification; do
  options=()
  [[ $profile == default ]] || options=(--features "$profile")
  cargo tree --manifest-path "$root/rust_hft/research-core/platform/Cargo.toml" \
    -p hft-research-platform "${options[@]}" --locked --edges normal --prefix none >"$work/control.tree"
  if grep -E '^(alpha-(domain|engine|harness|store|onnx-evaluator) |burn|ort |hft-(collector|research-ml|research-agent-improvement|data|backtest|cex-research-worker) |parquet )' "$work/control.tree"; then
    echo 'control pulls scientific domain, acquisition, training or Parquet' >&2; exit 1
  fi
done
cargo tree --manifest-path "$root/rust_hft/shared/Cargo.toml" \
  -p hft-research-agent-contracts --locked --edges normal --prefix none >"$work/agent-contracts.tree"
if grep -E '^(alpha-|burn|ort |parquet |tokio |reqwest |axum |sqlx|hft-(collector|research-platform|research-ml|data|execution-adapter-[a-z-]+) )' "$work/agent-contracts.tree"; then
  echo 'researcher contracts pull science, transport, database or execution' >&2; exit 1
fi
cargo tree --manifest-path "$root/rust_hft/research-core/Cargo.toml" \
  -p hft-research-agent-improvement --locked --edges normal --prefix none >"$work/agent-improvement.tree"
if grep -E '^(alpha-|burn|ort |parquet |tokio |reqwest |axum |sqlx|hft-(collector|research-platform|research-ml|data|execution-adapter-[a-z-]+) )' "$work/agent-improvement.tree"; then
  echo 'configuration consumer pulls scientific engines, control, transport or execution' >&2; exit 1
fi
for features in formula-strategy,binance full; do
  cargo tree --manifest-path "$root/rust_hft/runtime/Cargo.toml" -p hft-live \
    --no-default-features --features "$features" --locked --edges normal --prefix none >"$work/live.tree"
  if grep -E '^(alpha-(domain|engine|store|onnx-evaluator|harness) |burn |hft-(collector|research-platform|research-ml) )' "$work/live.tree"; then
    echo 'live imports research control, evaluation or training' >&2; exit 1
  fi
done
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
for package in hft-research-artifacts hft-research-dispatch-io hft-prediction-research-worker hft-prediction-research-operator; do
  case $package in hft-research-artifacts|hft-research-dispatch-io) owner=shared ;; *) owner=prediction-markets ;; esac
  cargo tree --manifest-path "$root/rust_hft/$owner/Cargo.toml" \
    -p "$package" --locked --edges normal --prefix none >"$work/worker.tree"
  if grep -E '^(alpha-(domain|engine|harness|onnx-evaluator) |burn |ort |hft-(collector|backtest|research-platform|research-ml|execution-adapter-[a-z-]+) )' "$work/worker.tree"; then
    echo 'Prediction/artifact worker imports acquisition, CEX research, control, training or execution' >&2; exit 1
  fi
  if [[ $package == hft-research-artifacts || $package == hft-prediction-research-worker ]] &&
    grep -E '^hft-research-dispatch-io ' "$work/worker.tree"; then
    echo 'artifact transport or scientific worker imports cluster operations' >&2; exit 1
  fi
done
cargo tree --manifest-path "$root/rust_hft/runtime/Cargo.toml" \
  -p hft-strategy-probability-reversal --locked --edges normal --prefix none >"$work/probability.tree"
if grep -E '^(alpha-(domain|engine|harness|store|onnx-evaluator) |burn |ort |sqlx-|axum |reqwest |hft-(collector|research-platform|research-ml|execution-adapter-[a-z-]+) )' "$work/probability.tree"; then
  echo 'fixed probability strategy imports science, control, transport or execution' >&2; exit 1
fi
printf 'workspace ownership, scoped commands, feature rejection and thin closures passed\n'
