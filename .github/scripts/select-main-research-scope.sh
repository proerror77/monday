#!/usr/bin/env bash
set -euo pipefail
head=${1:?source required} output=${2:?plan output required} metadata=${3:-}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
bash "$script_dir/read-research-publish-baseline.sh" "$head" "$work/base"
current=${SELECTED_RESEARCH_PRODUCT:-none}
jobs=${SELECTED_JOBS:-,,}
[[ $current == none || $current == runner || $current == controller || $current == paired ]] || exit 2
[[ $jobs =~ ^,[a-z0-9/-]*(,[a-z0-9/-]+)*,$ ]] || exit 2
accumulated=none
for target in runner controller; do
  base=$(jq -er --arg target "$target" '.[$target]' "$work/base")
  if [[ $base == BOOTSTRAP ]]; then impact=paired
  else
    [[ $base =~ ^[0-9a-f]{40}$ ]] || exit 1
    # Only deployed image inputs belong here. Governance/history remain in their
    # per-change contract jobs and are not permanent rebuild inputs.
    git diff --no-renames --name-only "$base" "$head" -- rust_hft/ deployment/aliyun/research/ \
      .github/scripts/build-research-release.sh .github/scripts/research-release-products.sh \
      .github/scripts/research-release-products.json .github/scripts/capture-research-build-inputs.sh \
      .github/scripts/research-image-smoke.sh .github/scripts/research-release-bundle.rb .cargo/ >"$work/paths"
    args=()
    [[ -z $metadata ]] || args+=(--metadata "$metadata")
    : >"$work/plan"
    GITHUB_REF=refs/heads/main bash "$script_dir/select-rust-ci-scope.sh" --event push --base "$base" --head "$head" \
      --changed-files "$work/paths" --output "$work/plan" "${args[@]}"
    impact=$(sed -n 's/^research_product=//p' "$work/plan")
    [[ $impact == none || $impact == paired || $impact == controller || $impact == runner ]] || exit 1
  fi
  if [[ $target == runner && ( $impact == paired || $impact == runner ) ]]; then accumulated=paired
  elif [[ $target == controller && $impact != none && $accumulated != paired ]]; then accumulated=controller
  fi
done
product=$current
if [[ $accumulated == paired || $current == paired || $current == runner ]]; then product=paired
elif [[ $accumulated == controller ]]; then product=controller
fi
if [[ $product != none ]]; then
  for job in ploy/research-image-binaries ploy/research-image-smoke; do
    if [[ $jobs != *,$job,* ]]; then
      if [[ $jobs == ,, ]]; then jobs=",$job,"; else jobs="${jobs%,},$job,"; fi
    fi
  done
fi
printf 'research_base_sha=%s\nresearch_product=%s\njobs=%s\n' "$(jq -c . "$work/base")" "$product" "$jobs" >>"$output"
