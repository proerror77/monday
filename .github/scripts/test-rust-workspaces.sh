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
cargo tree --manifest-path "$root/rust_hft/research-core/Cargo.toml" \
  -p alpha-harness --locked --edges normal --prefix none >"$work/cex-operator.tree"
if grep -E '^(burn|ort |alpha-onnx-evaluator |hft-(infer-onnx|research-ml) )' "$work/cex-operator.tree"; then
  echo 'CEX operator pulls a training or ONNX implementation' >&2; exit 1
fi
cargo tree --manifest-path "$root/rust_hft/research-core/Cargo.toml" \
  -p hft-cex-research-worker --locked --edges normal --prefix none >"$work/cex-worker.tree"
grep -q '^hft-research-ml ' "$work/cex-worker.tree"
grep -q '^burn ' "$work/cex-worker.tree"
for profile in default db full; do
  options=()
  [[ $profile == default ]] || options=(--features db)
  cargo tree --manifest-path "$root/rust_hft/prediction-markets/Cargo.toml" \
    -p ploy-research "${options[@]}" --locked --edges normal --prefix none >"$work/prediction.tree"
  if grep -E '^(alpha-(domain|engine|harness|onnx-evaluator) |hft-(cex-research-input|collector|backtest|research-platform) )' "$work/prediction.tree"; then
    echo 'prediction research pulls the CEX input, evaluator or control chain' >&2; exit 1
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
