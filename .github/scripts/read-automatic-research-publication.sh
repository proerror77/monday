#!/usr/bin/env bash
# GET-only. Rejection emits no environment matrix or reusable admission output.
set -euo pipefail
products=${1:?canonical products}
source_sha=${2:?exact source SHA}
output=${3:?new admission path}
: "${GITHUB_REPOSITORY:?missing repository}"
: "${GITHUB_RUN_ID:?missing native run ID}"
: "${GITHUB_RUN_ATTEMPT:?missing native attempt}"
[[ $GITHUB_REPOSITORY =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ && $source_sha =~ ^[0-9a-f]{40}$ ]]
[[ $GITHUB_REF == refs/heads/main && ! -e $output ]]
[[ $GITHUB_RUN_ID =~ ^[1-9][0-9]*$ && $GITHUB_RUN_ATTEMPT =~ ^[1-9][0-9]*$ ]]
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
printf '%s' "${MONDAY_RESEARCH_AUTOMATIC_PUBLICATION:-null}" > "$work/config.json"
printf '%s' "${MONDAY_RESEARCH_RELEASE_POLICY:-null}" > "$work/policy.json"
jq -e --arg products "$products" '
  .schema == "monday.automatic_research_publication.v1"
  and ($products | split(",") | length > 0 and length <= 3 and . == (unique | sort)
    and all(.[]; . == "cex-runner" or . == "controller" or . == "prediction-runner"))
' "$work/config.json" >/dev/null
gh api --method GET "repos/$GITHUB_REPOSITORY" > "$work/repository.json"
gh api --method GET "repos/$GITHUB_REPOSITORY/git/ref/heads/main" > "$work/main.json"
gh api --method GET "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID" > "$work/run.json"
gh api --method GET "repos/$GITHUB_REPOSITORY/actions/oidc/customization/sub" > "$work/oidc.json"
bindings=()
IFS=, read -r -a selected <<< "$products"
for product in "${selected[@]}"; do
  name=$(jq -er --arg product "$product" '.products[$product].name' "$work/config.json")
  case "$product/$name" in
    cex-runner/monday-research-cex|controller/monday-research-controller|prediction-runner/monday-research-prediction) ;;
    *) echo 'unconfigured product environment' >&2; exit 1 ;;
  esac
  gh api --method GET "repos/$GITHUB_REPOSITORY/environments/$name" > "$work/environment.json"
  gh api --method GET "repos/$GITHUB_REPOSITORY/environments/$name/deployment-branch-policies?per_page=100" > "$work/branches.json"
  jq -e --arg product "$product" -f "$script_dir/select-research-oss-policy.jq" "$work/policy.json" > "$work/selected-policy.json"
  jq -n --arg product "$product" --slurpfile env "$work/environment.json" \
    --slurpfile branches "$work/branches.json" --slurpfile policy "$work/selected-policy.json" \
    '{product:$product,environment:$env[0],branches:$branches[0],policy:$policy[0]}' > "$work/$product.json"
  bindings+=("$work/$product.json")
done
jq -s '.' "${bindings[@]}" > "$work/environments.json"
jq -n --arg products "$products" --arg source "$source_sha" --arg repo "$GITHUB_REPOSITORY" \
  --argjson run_id "$GITHUB_RUN_ID" --argjson attempt "$GITHUB_RUN_ATTEMPT" \
  --slurpfile config "$work/config.json" --slurpfile repository "$work/repository.json" \
  --slurpfile main "$work/main.json" --slurpfile run "$work/run.json" --slurpfile oidc "$work/oidc.json" \
  --slurpfile environments "$work/environments.json" \
  '{products:$products,source_sha:$source,repository_name:$repo,run_id:$run_id,run_attempt:$attempt,
    config:$config[0],repository:$repository[0],main:$main[0],run:$run[0],oidc:$oidc[0],environments:$environments[0]}' \
  > "$work/bundle.json"
jq -e -f "$script_dir/automatic-research-publication.jq" "$work/bundle.json" > "$work/admission.json"
# Recheck main after environment reads, before any environment name is emitted.
gh api --method GET "repos/$GITHUB_REPOSITORY/git/ref/heads/main" \
  | jq -e --arg source "$source_sha" '.object.sha == $source' >/dev/null
cp "$work/admission.json" "$output"
