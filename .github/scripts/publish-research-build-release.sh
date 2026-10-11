#!/usr/bin/env bash
# Dedicated CI issuer. ACK owns independent import. No science or provisioning.
set +x
set -euo pipefail
mode=${1:?expected check-presence, check-public-policy, check-config or publish}
if [[ $mode == check-presence ]]; then
  # This cheap check receives only GitHub presence booleans, never credentials.
  # The native signer/policy/TLS check below remains the publication authority.
  missing=()
  [[ ${MONDAY_RELEASE_POLICY_PRESENT:-false} == true ]] || missing+=("variable MONDAY_RESEARCH_RELEASE_POLICY")
  [[ ${MONDAY_RELEASE_SIGNING_KEY_PRESENT:-false} == true ]] || missing+=("secret MONDAY_RESEARCH_RELEASE_SIGNING_KEY")
  [[ ${MONDAY_RELEASE_OSS_PRESENT:-false} == true ]] || missing+=("OSS OIDC settings in variable MONDAY_RESEARCH_RELEASE_POLICY")
  if ((${#missing[@]})); then
    printf 'Research publication configuration missing: %s\n' "${missing[@]}" >&2
    printf 'Configure the named repository settings through the approved operator; native signing and TLS admission remain required.\n' >&2
    exit 1
  fi
  printf 'Research publication settings are present; native signer, policy and TLS validation remain required.\n'
  exit 0
fi
: "${MONDAY_RELEASE_POLICY_JSON:?operator public publisher policy required}"
root=$(cd "$(dirname "$0")/../.." && pwd)
: "${PRODUCT:?image product required}"
select_public_policy() {
  printf '%s' "$MONDAY_RELEASE_POLICY_JSON" |
    jq -es 'if length == 1 and (.[0] | type) == "object" then .[0] else error("one public research publisher policy object required") end' |
    jq -e --arg product "$PRODUCT" -f "$root/.github/scripts/select-research-oss-policy.jq" |
    jq -e --arg product "$PRODUCT" '
      def text: type == "string" and length > 0;
      if .trust.schema == 1 and
         .trust.producer_workflow_path == ".github/workflows/acr-publish.yml" and
         (.key_id | text) and (.trust.keys | type) == "object" and
         (.trust.keys[.key_id] | type == "string" and test("^[0-9a-f]{64}$")) and
         (.builder_image | type == "string" and test("@sha256:[0-9a-f]{64}$")) and
         (.image_repositories | type == "object" and length > 0) and
         (.image_repositories[$product] | text) and
         all([.oss.bucket, .oss.role_arn, .oss.oidc_provider_arn][]; text)
      then . else error("invalid public research publisher policy structure for selected product") end
    '
}
if [[ $mode == check-public-policy ]]; then
  # Public structure only. No issuer, private key, OIDC exchange or network call.
  select_public_policy >/dev/null
  printf 'Public research publication policy structure is valid for %s; native signing, OIDC and TLS validation remain required.\n' "$PRODUCT"
  exit 0
fi
issuer=${RUNNER_TEMP:?prebuilt native issuer directory required}/research-release-issuer-target/debug/research-release-publisher
capability=${RUNNER_TEMP}/research-release-issuer-target/debug/research-release-capability
test -x "$issuer"
test -x "$capability"
work=$(mktemp -d)
umask 077
ledger=''
finish() {
  result=$?
  trap - EXIT
  if [[ -n $ledger ]]; then
    summary="$RUNNER_TEMP/research-publication-budget-usage.json"
    if "$issuer" budget-summary "$ledger" >"$work/usage.json" 2>/dev/null; then
      mv "$work/usage.json" "$summary"
    else
      printf '%s\n' '{"schema":"monday.oss-publication-budget-usage.v1","usage_known":false,"reason":"private ledger unavailable or invalid"}' >"$summary"
      ((result != 0)) || result=1
    fi
  fi
  rm -rf "$work"
  exit "$result"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
select_public_policy >"$work/policy.json"
unset MONDAY_RELEASE_GATEWAY_TOKEN
context="$RUNNER_TEMP/research-release-context.json"
make_context() {
  : "${SOURCE_REVISION:?source required}" "${PRODUCER_RUN:?software producer required}"
  : "${PRODUCT:?image product required}" "${IMAGE_REPOSITORY:?image name required}"
  : "${GITHUB_REPOSITORY:?repository required}" "${GITHUB_RUN_ID:?publisher run required}" "${GITHUB_RUN_ATTEMPT:?publisher attempt required}"
  job=$(gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs?per_page=100" | jq -er --arg name "Publish $IMAGE_REPOSITORY" '[.[].jobs[]?|select(.name==$name)] | if length==1 then .[0].id else error("ambiguous publisher job") end')
  [[ $job =~ ^[1-9][0-9]*$ ]]
  jq -n --arg repository "$GITHUB_REPOSITORY" --arg source "$SOURCE_REVISION" --arg product "$PRODUCT" --arg image_repository "$1" \
    --argjson software_run "$PRODUCER_RUN" --argjson run "$GITHUB_RUN_ID" --argjson attempt "$GITHUB_RUN_ATTEMPT" --argjson job "$job" \
    '{repository:$repository,source_sha:$source,product:$product,image_repository:$image_repository,software_run_id:$software_run,publisher_run_id:$run,publisher_run_attempt:$attempt,publisher_job_id:$job}' >"$context"
}
case "$mode" in
  check-config)
    : "${MONDAY_RELEASE_SIGNING_KEY:?independent private signing key required}"
    [[ $MONDAY_RELEASE_SIGNING_KEY =~ ^[0-9a-f]{64}$ ]]
    printf '%s' "$MONDAY_RELEASE_SIGNING_KEY" >"$work/key"
    unset MONDAY_RELEASE_SIGNING_KEY
    : "${GITHUB_REPOSITORY:?repository required}" "${PRODUCT:?image product required}"
    : "${PUBLISH_IMAGE_REPOSITORY:?exact selected OCI repository required}" "${RUNNER_TEMP:?authenticated software directory required}"
    make_context "$PUBLISH_IMAGE_REPOSITORY"
    : "${SOFTWARE_PRODUCTS:?actual software product selection required}"
    budget_dir="$RUNNER_TEMP/research-oss-budget/$GITHUB_RUN_ID/$GITHUB_RUN_ATTEMPT/$PRODUCT"
    mkdir -p -m 700 "$budget_dir"
    ledger="$budget_dir/ledger.jsonl"
    "$issuer" budget-plan "$root" "$context" "$SOFTWARE_PRODUCTS" "$work/policy.json" >"$budget_dir/plan.json"
    "$issuer" budget-init "$work/policy.json" "$context" "$budget_dir/plan.json" "$RUNNER_TEMP/research-publication-native-budget.json" "$ledger" >"$budget_dir/init.json"
    "$capability" oss-source "$work/policy.json" "$context" - "$ledger" "$work/session"
    "$issuer" oss-check-config "$work/policy.json" "$work/key" "$RUNNER_TEMP/research-release/research-image-release.json" "$GITHUB_REPOSITORY" "$PRODUCT" "$PUBLISH_IMAGE_REPOSITORY" "$SOURCE_REVISION" "$work/session"
    ;;
  publish)
    : "${MONDAY_RELEASE_SIGNING_KEY:?independent private signing key required}"
    [[ $MONDAY_RELEASE_SIGNING_KEY =~ ^[0-9a-f]{64}$ ]]
    : "${SOURCE_REVISION:?source required}" "${PRODUCER_RUN:?software producer required}"
    : "${SOFTWARE_PRODUCTS:?compiled product selection required}" "${PRODUCT:?image product required}"
    : "${IMAGE:?immutable OCI identity required}" "${IMAGE_REPOSITORY:?image repository required}"
    : "${GITHUB_REPOSITORY:?repository required}" "${GITHUB_RUN_ID:?publisher run required}" "${GITHUB_RUN_ATTEMPT:?publisher attempt required}"
    printf '%s' "$MONDAY_RELEASE_SIGNING_KEY" >"$work/key"
    unset MONDAY_RELEASE_SIGNING_KEY
    make_context "${IMAGE%@sha256:*}"
    ledger="$RUNNER_TEMP/research-oss-budget/$GITHUB_RUN_ID/$GITHUB_RUN_ATTEMPT/$PRODUCT/ledger.jsonl"
    jq -n --arg source "$SOURCE_REVISION" --argjson software_run "$PRODUCER_RUN" --arg products "$SOFTWARE_PRODUCTS" --arg product "$PRODUCT" --arg image "$IMAGE" --argjson run "$GITHUB_RUN_ID" --argjson attempt "$GITHUB_RUN_ATTEMPT" --argjson job "$job" \
      '{source_sha:$source,software_run_id:$software_run,software_products:$products,product:$product,image:$image,publisher_run_id:$run,publisher_run_attempt:$attempt,publisher_job_id:$job}' >"$work/request.json"
    native=("$issuer")
    "${native[@]}" plan "$root" "$work/request.json" "$work/policy.json" >"${RUNNER_TEMP:?}/research-build-plan.json"
    "$capability" oss-publish "$work/policy.json" "$context" "$RUNNER_TEMP/research-build-plan.json" "$ledger" "$work/session"
    "${native[@]}" oss-publish "$root" "$work/request.json" "$work/policy.json" "$work/key" "$work/session" >"$RUNNER_TEMP/research-build-artifacts.json"
    ;;
  *) exit 2 ;;
esac
