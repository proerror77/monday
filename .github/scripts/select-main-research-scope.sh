#!/usr/bin/env bash
set -euo pipefail
head=${1:?source required} output=${2:?plan output required} metadata=${3:-}
carry_mode=${RESEARCH_CARRY_MODE:-always}
[[ $carry_mode == always || $carry_mode == defer-unconfigured ]] || exit 2
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
bash "$script_dir/read-research-publish-baseline.sh" "$head" "$work/base"
current=${SELECTED_RESEARCH_PRODUCT:-none}
jobs=${SELECTED_JOBS:-,,}
[[ $current == none ]] || current=$(bash "$script_dir/research-release-products.sh" normalize "$current")
[[ $jobs =~ ^,[a-z0-9/-]*(,[a-z0-9/-]+)*,$ ]] || exit 2
accumulated=none
for target in cex-runner controller prediction-runner; do
  base=$(jq -er --arg target "$target" '.[$target]' "$work/base")
  if [[ $base == BOOTSTRAP ]]; then impact=$target
  else
    [[ $base =~ ^[0-9a-f]{40}$ ]] || exit 1
    # Only deployed image inputs belong here. Governance/history remain in their
    # per-change contract jobs and are not permanent rebuild inputs.
    git diff --no-renames --name-only "$base" "$head" -- rust_hft/ deployment/aliyun/research/ \
      .github/scripts/build-research-release.sh .github/scripts/build-research-recipes.sh \
      .github/scripts/research-cache-layout.sh \
      .github/scripts/vendor/tomlrb/ \
      .github/scripts/research-release-products.sh \
      .github/scripts/research-release-products.json .github/scripts/capture-research-build-inputs.sh \
      .github/scripts/research-image-smoke.sh .github/scripts/research-release-bundle.rb \
      .github/scripts/verify-research-product-image.sh .cargo/ >"$work/paths"
    args=()
    [[ -z $metadata ]] || args+=(--metadata "$metadata")
    : >"$work/plan"
    GITHUB_REF=refs/heads/main bash "$script_dir/select-rust-ci-scope.sh" --event push --base "$base" --head "$head" \
      --changed-files "$work/paths" --output "$work/plan" "${args[@]}"
    impact=$(sed -n 's/^research_product=//p' "$work/plan")
    [[ $impact == none ]] || impact=$(bash "$script_dir/research-release-products.sh" normalize "$impact")
  fi
  if [[ $impact != none ]] && bash "$script_dir/research-release-products.sh" contains "$impact" "$target"; then
    accumulated=$(bash "$script_dir/research-release-products.sh" merge "$accumulated" "$target")
  fi
done
policy_state=unchecked
if [[ $carry_mode == defer-unconfigured ]]; then
  if [[ -z ${MONDAY_RELEASE_POLICY_JSON:-} ]]; then policy_state=unconfigured
  else
    # Only confirmed absent configuration defers work. Invalid policy retains
    # the existing carry plan and still requires native publication validation.
    policy_state=$(printf '%s' "$MONDAY_RELEASE_POLICY_JSON" | jq -esr '
      if length != 1 or (.[0] | type) != "object" then "invalid"
      elif .[0].oss_by_product == null or .[0].oss_by_product == {} then "unconfigured"
      elif (.[0].oss_by_product | type) == "object" then "configured"
      else "invalid" end' 2>/dev/null) || policy_state=invalid
  fi
fi
deferred=none
product=$(bash "$script_dir/research-release-products.sh" merge "$current" "$accumulated")
if [[ $policy_state == unconfigured ]]; then
  # Keep every direct-source product and job. The baseline remains unchanged.
  product=$current
  deferred=$(jq -nr --arg pending "$accumulated" --arg current "$current" '
    def products: if . == "none" then [] else split(",") end;
    (($pending | products) - ($current | products)) | join(",") | if . == "" then "none" else . end')
  printf 'Research carry deferred: %s; direct-source products retained: %s; publication baseline unchanged.\n' "$deferred" "$current" >&2
elif [[ $policy_state == invalid ]]; then
  printf 'Invalid public research policy; retaining cumulative builds for native publication validation.\n' >&2
fi
if [[ $product != none ]]; then
  for job in ploy/research-image-binaries ploy/research-image-smoke; do
    if [[ $jobs != *,$job,* ]]; then
      if [[ $jobs == ,, ]]; then jobs=",$job,"; else jobs="${jobs%,},$job,"; fi
    fi
  done
fi
printf 'research_base_sha=%s\nresearch_product=%s\nresearch_pending_product=%s\nresearch_deferred_product=%s\nresearch_carry_policy=%s\njobs=%s\n' \
  "$(jq -c . "$work/base")" "$product" "$accumulated" "$deferred" "$policy_state" "$jobs" >>"$output"
