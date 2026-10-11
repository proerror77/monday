#!/usr/bin/env bash
# Archive-only admission. Absent/unapproved configuration leaves images usable.
set -euo pipefail
source=${1:?source required} products=${2:?products required} matrix=${3:?source matrix required} output=${4:?output required}
script_dir=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
pending() {
  printf 'ready=false\nmatrix={"include":[]}\n' >>"$output"
  printf 'Signed Build archive pending: %s. OCI delivery has independent evidence.\n' "$1"
}
if [[ -z ${MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY:-} ||
      -z ${MONDAY_RESEARCH_AUTOMATIC_PUBLICATION:-} ||
      -z ${MONDAY_RESEARCH_RELEASE_POLICY:-} ||
      ${MONDAY_RELEASE_SIGNING_KEY_PRESENT:-false} != true ]]; then
  pending 'operating allowance, environment, signing or OSS configuration is unapproved'
  exit 0
fi
export MONDAY_RELEASE_POLICY_JSON=$MONDAY_RESEARCH_RELEASE_POLICY
storage_hours=$(jq -er '.storage_hours' <<<"$MONDAY_RESEARCH_PUBLICATION_OPERATIONS_POLICY") || { pending 'invalid operating allowance'; exit 0; }
if ! bash "$script_dir/research-publication-budget.sh" check-operations "$source" "$products" "$storage_hours" "$work/allowance-check.json"; then
  pending 'operating allowance is invalid, expired, exhausted or a retry'
  exit 0
fi
IFS=, read -r -a selected <<<"$products"
eligible=()
for product in "${selected[@]}"; do
  repository=$(case "$product" in cex-runner) echo research-runner;; controller) echo campaign-cycle-controller;; prediction-runner) echo prediction-research-runner;; *) exit 1;; esac)
  if ! ruby "$script_dir/research-oci-delivery.rb" archive-history "$source" "$product" "${ACR_REGISTRY:-fixture.invalid}/wildcard0923/$repository"; then
    printf 'Signed Build archive pending for %s: prior attempt or unreadable history requires reconciliation.\n' "$product"
    continue
  fi
  if ! PRODUCT="$product" bash "$script_dir/publish-research-build-release.sh" check-public-policy; then
    printf 'Signed Build archive pending for %s: selected OSS policy is invalid.\n' "$product"
    continue
  fi
  eligible+=("$product")
done
if ((${#eligible[@]} == 0)); then
  pending 'no selected product has an unused archive allowance'
  exit 0
fi
archive_products=$(IFS=,; printf '%s' "${eligible[*]}")
if ! bash "$script_dir/research-publication-budget.sh" admit-operations "$source" "$archive_products" "$storage_hours" "$work/allowance.json"; then
  pending 'eligible archive price target or allowance is invalid'
  exit 0
fi
bash "$script_dir/read-automatic-research-publication.sh" "$archive_products" "$source" "$work/admission.json"
selected_matrix=$(jq -cn --argjson source "$matrix" --slurpfile admission "$work/admission.json" '
  {include:[$source.include[] | select(.research_artifact == true) | . as $row
    | $admission[0].environments[] | select(.product == $row.product)
    | $row + {environment_name:.name,environment_id:.environment_id}]}')
[[ $(jq '.include | length' <<<"$selected_matrix") -eq ${#eligible[@]} ]]
printf 'ready=true\nmatrix=%s\narchive_products=%s\n' "$selected_matrix" "$archive_products" >>"$output"
