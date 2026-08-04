#!/usr/bin/env bash
set -euo pipefail

mode=${1:?expected create or verify}
release=${2:?expected release directory}
source_sha=${3:?expected source SHA}
run_id=${4:?expected workflow run id}
repo_root=${5:?expected rust_hft directory}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
manifest="$release/research-image-release.json"
target=x86_64-unknown-linux-gnu
binaries=(
  hft-backtest
  alpha-harness
  lob-pit-materializer
  binance-replay-parquet-materializer
  monday-prediction-research
  monday-prediction-evaluator
  monday-prediction-snapshot
)

[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { printf 'invalid source SHA: %s\n' "$source_sha" >&2; exit 1; }
[[ $run_id =~ ^[1-9][0-9]*$ ]] || { printf 'invalid workflow run id: %s\n' "$run_id" >&2; exit 1; }
test -f "$repo_root/Cargo.lock"
test -f "$repo_root/prediction-markets/Cargo.lock"

case "$mode" in
  create)
    test ! -e "$manifest"
    "$script_dir/verify-research-runner-binaries.sh" "$release/research-bin"
    binary_manifest='[]'
    for binary in "${binaries[@]}"; do
      digest=$(sha256sum "$release/research-bin/$binary" | awk '{print $1}')
      binary_manifest=$(jq -c --arg file "$binary" --arg sha256 "$digest" \
        '. + [{file:$file,sha256:$sha256}]' <<<"$binary_manifest")
    done
    jq -n \
      --arg source_sha "$source_sha" \
      --arg workflow_run_id "$run_id" \
      --arg target "$target" \
      --arg root_lock_sha256 "$(sha256sum "$repo_root/Cargo.lock" | awk '{print $1}')" \
      --arg prediction_lock_sha256 "$(sha256sum "$repo_root/prediction-markets/Cargo.lock" | awk '{print $1}')" \
      --argjson binaries "$binary_manifest" \
      '{schema:"monday.research-image-release.v1",
        source_sha:$source_sha,
        workflow_run_id:$workflow_run_id,
        target:$target,
        cargo_locks:{"Cargo.lock":$root_lock_sha256,
          "prediction-markets/Cargo.lock":$prediction_lock_sha256},
        binaries:$binaries}' >"$manifest"
    ;;
  verify)
    test "$(find "$release" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" -eq 2
    test -f "$manifest"
    "$script_dir/verify-research-runner-binaries.sh" "$release/research-bin"
    jq -e \
      --arg source_sha "$source_sha" \
      --arg workflow_run_id "$run_id" \
      --arg target "$target" \
      --arg root_lock_sha256 "$(sha256sum "$repo_root/Cargo.lock" | awk '{print $1}')" \
      --arg prediction_lock_sha256 "$(sha256sum "$repo_root/prediction-markets/Cargo.lock" | awk '{print $1}')" \
      '.schema == "monday.research-image-release.v1" and
       .source_sha == $source_sha and
       .workflow_run_id == $workflow_run_id and
       .target == $target and
       .cargo_locks == {"Cargo.lock":$root_lock_sha256,
         "prediction-markets/Cargo.lock":$prediction_lock_sha256} and
       (.binaries | length) == 7' "$manifest" >/dev/null
    for binary in "${binaries[@]}"; do
      expected=$(jq -er --arg file "$binary" \
        '.binaries | map(select(.file == $file)) | if length == 1 then .[0].sha256 else error("binary manifest mismatch") end' \
        "$manifest")
      actual=$(sha256sum "$release/research-bin/$binary" | awk '{print $1}')
      test "$actual" = "$expected"
    done
    ;;
  *) printf 'unsupported artifact mode: %s\n' "$mode" >&2; exit 2 ;;
esac
