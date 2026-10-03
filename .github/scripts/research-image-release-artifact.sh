#!/usr/bin/env bash
set -euo pipefail

mode=${1:?expected create or verify}
release=${2:?expected release directory}
source_sha=${3:?expected source SHA}
run_id=${4:?expected workflow run id}
repo_root=${5:?expected rust_hft directory}
attempt=${6:-${GITHUB_RUN_ATTEMPT:-}}
job_id=${7:-${MONDAY_RELEASE_JOB_ID:-}}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
manifest="$release/research-image-release.json"
target=x86_64-unknown-linux-gnu
binaries=(
  hft-backtest
  alpha-harness
  lob-pit-materializer
  binance-market-tape-slicer
  binance-replay-parquet-materializer
  research-orchestrator
  researchctl
  research-prepare
  clickhouse-analytics-materializer
  monday-prediction-research
  monday-prediction-evaluator
  monday-prediction-snapshot
)

[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { printf 'invalid source SHA: %s\n' "$source_sha" >&2; exit 1; }
[[ $run_id =~ ^[1-9][0-9]*$ ]] || { printf 'invalid workflow run id: %s\n' "$run_id" >&2; exit 1; }
[[ $attempt =~ ^[1-9][0-9]*$ ]] || { echo 'missing producer attempt' >&2; exit 1; }
# Consumers may take job_id from this same-run artifact, then independently
# validate the job via Actions before any publication. Cross-run consumers must
# pass the API-derived numeric job_id explicitly.
if [[ -z $job_id && $mode != create ]]; then job_id=$(jq -er '.workflow_job_id' "$manifest"); fi
[[ $job_id =~ ^[1-9][0-9]*$ ]] || { echo 'missing producer job' >&2; exit 1; }
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
    : "${MONDAY_BUILD_INPUTS_FILE:?compiler/native build inputs required}"
    jq -e ' .schema == "monday.compilation-inputs.v1" and .target == "x86_64-unknown-linux-gnu" ' "$MONDAY_BUILD_INPUTS_FILE" >/dev/null
    jq -n \
      --slurpfile build_inputs "$MONDAY_BUILD_INPUTS_FILE" \
      --arg source_sha "$source_sha" \
      --arg workflow_run_id "$run_id" \
      --argjson workflow_run_attempt "$attempt" \
      --argjson workflow_job_id "$job_id" \
      --arg target "$target" \
      --arg root_lock_sha256 "$(sha256sum "$repo_root/Cargo.lock" | awk '{print $1}')" \
      --arg prediction_lock_sha256 "$(sha256sum "$repo_root/prediction-markets/Cargo.lock" | awk '{print $1}')" \
      --argjson binaries "$binary_manifest" \
      '{schema:"monday.research-image-release.v2",
        source_sha:$source_sha,
        workflow_run_id:$workflow_run_id,
        workflow_run_attempt:$workflow_run_attempt,
        workflow_job_id:$workflow_job_id,
        target:$target,
        build_inputs:$build_inputs[0],
        cargo_locks:{"Cargo.lock":$root_lock_sha256,
          "prediction-markets/Cargo.lock":$prediction_lock_sha256},
        binaries:$binaries}' >"$manifest"
    ;;
  verify|verify-metadata)
    test "$(find "$release" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" -eq 2
    test -f "$manifest"
    # Verify bytes/provenance/file modes; the CI smoke job checks the same ELF files.
    "$script_dir/verify-research-runner-binaries.sh" "$release/research-bin"
    jq -e \
      --arg source_sha "$source_sha" \
      --arg workflow_run_id "$run_id" \
      --argjson workflow_run_attempt "$attempt" \
      --argjson workflow_job_id "$job_id" \
      --arg target "$target" \
      --argjson binary_count "${#binaries[@]}" \
      --arg root_lock_sha256 "$(sha256sum "$repo_root/Cargo.lock" | awk '{print $1}')" \
      --arg prediction_lock_sha256 "$(sha256sum "$repo_root/prediction-markets/Cargo.lock" | awk '{print $1}')" \
      '.schema == "monday.research-image-release.v2" and
       .source_sha == $source_sha and
       .workflow_run_id == $workflow_run_id and
       .workflow_run_attempt == $workflow_run_attempt and
       .workflow_job_id == $workflow_job_id and
       .target == $target and
       (.build_inputs | .schema == "monday.compilation-inputs.v1" and .target == $target and .profile == "release" and ([.compiler,.native,.flags,.profiles,.recipe,.locks.root,.locks.prediction] | all(.[];test("^[0-9a-f]{64}$")))) and
       .build_inputs.locks.root == $root_lock_sha256 and .build_inputs.locks.prediction == $prediction_lock_sha256 and
       .cargo_locks == {"Cargo.lock":$root_lock_sha256,
         "prediction-markets/Cargo.lock":$prediction_lock_sha256} and
       (.binaries | length) == $binary_count' "$manifest" >/dev/null
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
