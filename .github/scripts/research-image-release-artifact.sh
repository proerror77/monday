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
product=$(bash "$(dirname "${BASH_SOURCE[0]}")/research-release-products.sh" normalize "${8:-all}")
binaries=()
while IFS= read -r binary; do binaries+=("$binary"); done < <(bash "$script_dir/research-release-products.sh" binaries "$product")
[[ ${#binaries[@]} -gt 0 ]] || exit 2

[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { printf 'invalid source SHA: %s\n' "$source_sha" >&2; exit 1; }
[[ $run_id =~ ^[1-9][0-9]*$ ]] || { printf 'invalid workflow run id: %s\n' "$run_id" >&2; exit 1; }
[[ $attempt =~ ^[1-9][0-9]*$ ]] || { echo 'missing producer attempt' >&2; exit 1; }
# Consumers may take job_id from this same-run artifact, then independently
# validate the job via Actions before any publication. Cross-run consumers must
# pass the API-derived numeric job_id explicitly.
if [[ -z $job_id && $mode != create ]]; then job_id=$(jq -er '.workflow_job_id' "$manifest"); fi
[[ $job_id =~ ^[1-9][0-9]*$ ]] || { echo 'missing producer job' >&2; exit 1; }
locks=$("$script_dir/research-workspace-locks.sh" "$repo_root")
recipes=$(bash "$script_dir/research-release-products.sh" recipes "$product" | jq -s .)

case "$mode" in
  create)
    test ! -e "$manifest"
    "$script_dir/verify-research-runner-binaries.sh" "$release/research-bin" "$product"
    binary_manifest='[]'
    for binary in "${binaries[@]}"; do
      digest=$(sha256sum "$release/research-bin/$binary" | awk '{print $1}')
      binary_manifest=$(jq -c --arg file "$binary" --arg sha256 "$digest" \
        '. + [{file:$file,sha256:$sha256}]' <<<"$binary_manifest")
    done
    : "${MONDAY_BUILD_INPUTS_FILE:?compiler/native build inputs required}"
    jq -e --argjson recipes "$recipes" --argjson locks "$locks" ' .schema == "monday.compilation-inputs.v3" and .target == "x86_64-unknown-linux-gnu" and .locks == $locks and (.builder_image|test("@sha256:[0-9a-f]{64}$")) and .recipes == $recipes and (.workspace_profiles|length)>0 ' "$MONDAY_BUILD_INPUTS_FILE" >/dev/null
    jq -n \
      --slurpfile build_inputs "$MONDAY_BUILD_INPUTS_FILE" \
      --arg source_sha "$source_sha" \
      --arg workflow_run_id "$run_id" \
      --argjson workflow_run_attempt "$attempt" \
      --argjson workflow_job_id "$job_id" \
      --arg target "$target" \
      --arg product "$product" \
      --argjson locks "$locks" \
      --argjson binaries "$binary_manifest" \
      '{schema:"monday.research-image-release.v6",
        products:($product | split(",")),
        source_sha:$source_sha,
        workflow_run_id:$workflow_run_id,
        workflow_run_attempt:$workflow_run_attempt,
        workflow_job_id:$workflow_job_id,
        target:$target,
        build_inputs:$build_inputs[0],
        cargo_locks:$locks,
        binaries:$binaries}' >"$manifest"
    ;;
  verify|verify-metadata)
    test "$(find "$release" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" -eq 2
    test -f "$manifest"
    # Verify bytes/provenance/file modes; the CI smoke job checks the same ELF files.
    "$script_dir/verify-research-runner-binaries.sh" "$release/research-bin" "$product"
    jq -e \
      --arg source_sha "$source_sha" \
      --arg workflow_run_id "$run_id" \
      --argjson workflow_run_attempt "$attempt" \
      --argjson workflow_job_id "$job_id" \
      --arg target "$target" \
      --arg product "$product" \
      --argjson binary_count "${#binaries[@]}" \
      --argjson locks "$locks" \
      --argjson recipes "$recipes" \
      '.schema == "monday.research-image-release.v6" and
       .products == ($product | split(",")) and
       .source_sha == $source_sha and
       .workflow_run_id == $workflow_run_id and
       .workflow_run_attempt == $workflow_run_attempt and
       .workflow_job_id == $workflow_job_id and
       .target == $target and
       (.build_inputs | .schema == "monday.compilation-inputs.v3" and .target == $target and .profile == "release" and ([.compiler,.native,.flags,.profiles,.recipe] + [.locks[]] | all(.[];test("^[0-9a-f]{64}$")))) and
       (.build_inputs.builder_image|test("@sha256:[0-9a-f]{64}$")) and
       (.build_inputs.recipes == $recipes) and
       (.build_inputs.workspace_profiles|length)>0 and
       .build_inputs.locks == $locks and .cargo_locks == $locks and
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
