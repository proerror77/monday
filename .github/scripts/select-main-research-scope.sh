#!/usr/bin/env bash
set -euo pipefail
head=${1:?source required} output=${2:?plan output required} metadata=${3:-}
carry_mode=${RESEARCH_CARRY_MODE:-always}
[[ $carry_mode == always || $carry_mode == defer-unconfigured ]] || exit 2
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
bash "$script_dir/read-research-publish-baseline.sh" "$head" "$work/base" images
bash "$script_dir/read-research-publish-baseline.sh" "$head" "$work/archive-base" builds
current=${SELECTED_RESEARCH_PRODUCT:-none}
jobs=${SELECTED_JOBS:-,,}
[[ $current == none ]] || current=$(bash "$script_dir/research-release-products.sh" normalize "$current")
[[ $jobs =~ ^,[a-z0-9/-]*(,[a-z0-9/-]+)*,$ ]] || exit 2
pending_since() {
  local baseline=$1 accumulated=none target base impact
  local -a args
  for target in cex-runner controller prediction-runner; do
    base=$(jq -er --arg target "$target" '.[$target]' "$baseline") || return 1
    if [[ $base == BOOTSTRAP ]]; then impact=$target
    else
      [[ $base =~ ^[0-9a-f]{40}$ ]] || return 1
      # Governance/history are not permanent image rebuild inputs.
      git diff --no-renames --name-only "$base" "$head" -- rust_hft/ deployment/aliyun/research/ \
        .github/scripts/build-research-release.sh .github/scripts/build-research-recipes.sh \
        .github/scripts/research-cache-layout.sh .github/scripts/vendor/tomlrb/ \
        .github/scripts/research-release-products.sh .github/scripts/research-release-products.json \
        .github/scripts/capture-research-build-inputs.sh .github/scripts/research-image-smoke.sh \
        .github/scripts/research-release-bundle.rb .github/scripts/verify-research-product-image.sh \
        .cargo/ >"$work/paths" || return 1
      args=()
      [[ -z $metadata ]] || args+=(--metadata "$metadata")
      : >"$work/plan"
      GITHUB_REF=refs/heads/main bash "$script_dir/select-rust-ci-scope.sh" --event push --base "$base" --head "$head" \
        --changed-files "$work/paths" --output "$work/plan" "${args[@]}" || return 1
      impact=$(sed -n 's/^research_product=//p' "$work/plan")
      [[ $impact == none ]] || impact=$(bash "$script_dir/research-release-products.sh" normalize "$impact") || return 1
    fi
    if [[ $impact != none ]] && bash "$script_dir/research-release-products.sh" contains "$impact" "$target"; then
      accumulated=$(bash "$script_dir/research-release-products.sh" merge "$accumulated" "$target") || return 1
    fi
  done
  printf '%s\n' "$accumulated"
}
accumulated=$(pending_since "$work/base")
archive_pending=$(pending_since "$work/archive-base")
archive_selected=none
archive_policy=unapproved
# Only an approved, unexpired fixed source allocation can schedule extra
# current-main software for a missing signed Build. This read is offline and
# supplies no native budget or credentials; publisher readiness still owns all
# environment/history/signing admission. Image carry is always independent.
if [[ $archive_pending != none && -n ${MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY:-} ]]; then
  allowed=$(jq -er '.products | if type=="array" then join(",") else error("products required") end' \
    <<<"$MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY") || allowed=none
  storage_hours=$(jq -er '.storage_hours' <<<"$MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY") || storage_hours=0
  if [[ $allowed != none ]] && bash "$script_dir/research-publication-budget.sh" check-operations-scope \
      "$head" "$allowed" "$storage_hours" "$work/archive-scope.json"; then
    archive_policy=approved
    for target in cex-runner controller prediction-runner; do
      if bash "$script_dir/research-release-products.sh" contains "$allowed" "$target" &&
         bash "$script_dir/research-release-products.sh" contains "$archive_pending" "$target"; then
        archive_selected=$(bash "$script_dir/research-release-products.sh" merge "$archive_selected" "$target")
      fi
    done
  fi
fi
# Image delivery has no OSS prerequisite. The old mode remains a harmless
# compatibility spelling for callers that have not yet moved to `always`.
policy_state=unchecked
deferred=none
pending=$(bash "$script_dir/research-release-products.sh" merge "$accumulated" "$archive_selected")
product=$(bash "$script_dir/research-release-products.sh" merge "$current" "$pending")
if [[ $product != none ]]; then
  for job in ploy/research-image-binaries ploy/research-image-smoke; do
    if [[ $jobs != *,$job,* ]]; then
      if [[ $jobs == ,, ]]; then jobs=",$job,"; else jobs="${jobs%,},$job,"; fi
    fi
  done
fi
printf 'research_base_sha=%s\nresearch_archive_base_sha=%s\nresearch_product=%s\nresearch_pending_product=%s\nresearch_image_pending_product=%s\nresearch_archive_pending_product=%s\nresearch_archive_selected_product=%s\nresearch_archive_carry_policy=%s\nresearch_deferred_product=%s\nresearch_carry_policy=%s\njobs=%s\n' \
  "$(jq -c . "$work/base")" "$(jq -c . "$work/archive-base")" "$product" "$pending" "$accumulated" \
  "$archive_pending" "$archive_selected" "$archive_policy" "$deferred" "$policy_state" "$jobs" >>"$output"
