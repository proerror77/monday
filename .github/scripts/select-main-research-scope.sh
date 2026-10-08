#!/usr/bin/env bash
set -euo pipefail
head=${1:?source required} output=${2:?plan output required} metadata=${3:-}
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
product=$(bash "$script_dir/research-release-products.sh" merge "$current" "$accumulated")
if [[ $product != none ]]; then
  for job in ploy/research-image-binaries ploy/research-image-smoke; do
    if [[ $jobs != *,$job,* ]]; then
      if [[ $jobs == ,, ]]; then jobs=",$job,"; else jobs="${jobs%,},$job,"; fi
    fi
  done
fi
printf 'research_base_sha=%s\nresearch_product=%s\njobs=%s\n' "$(jq -c . "$work/base")" "$product" "$jobs" >>"$output"
