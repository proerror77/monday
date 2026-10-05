#!/usr/bin/env bash
# Dedicated CI issuer/importer. No science, provisioning or service signing keys.
set +x
set -euo pipefail
mode=${1:?expected check-config, publish or import}
: "${MONDAY_RELEASE_POLICY_JSON:?operator public publisher policy required}"
: "${MONDAY_RELEASE_GATEWAY:?scoped HTTPS artifact gateway required}"
: "${MONDAY_RELEASE_GATEWAY_TOKEN:?exact Build/source capability required}"
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
umask 077
printf '%s' "$MONDAY_RELEASE_POLICY_JSON" >"$work/policy.json"
printf '%s' "$MONDAY_RELEASE_GATEWAY_TOKEN" >"$work/token"
unset MONDAY_RELEASE_GATEWAY_TOKEN
jq -e '.trust.schema==1 and .trust.producer_workflow_path==".github/workflows/acr-publish.yml" and (.trust.keys[.key_id]|test("^[0-9a-f]{64}$")) and (.builder_image|test("@sha256:[0-9a-f]{64}$")) and (.image_repositories|length)>0' "$work/policy.json" >/dev/null
case "$mode" in
  check-config)
    : "${MONDAY_RELEASE_SIGNING_KEY:?independent private signing key required}"
    [[ $MONDAY_RELEASE_SIGNING_KEY =~ ^[0-9a-f]{64}$ ]]
    [[ $MONDAY_RELEASE_GATEWAY == https://*/ ]]
    printf '%s' "$MONDAY_RELEASE_SIGNING_KEY" >"$work/key"
    unset MONDAY_RELEASE_SIGNING_KEY
    cargo run --manifest-path "$root/rust_hft/research-core/platform/Cargo.toml" --locked --features publisher --bin research-release-publisher -- check-config "$work/policy.json" "$work/key" "$MONDAY_RELEASE_GATEWAY" "$work/token"
    ;;
  publish)
    : "${MONDAY_RELEASE_SIGNING_KEY:?independent private signing key required}"
    [[ $MONDAY_RELEASE_SIGNING_KEY =~ ^[0-9a-f]{64}$ ]]
    : "${SOURCE_REVISION:?source required}" "${PRODUCER_RUN:?software producer required}"
    : "${SOFTWARE_PRODUCTS:?compiled product selection required}" "${PRODUCT:?image product required}"
    : "${IMAGE:?immutable OCI identity required}" "${IMAGE_REPOSITORY:?image repository required}"
    : "${GITHUB_REPOSITORY:?repository required}" "${GITHUB_RUN_ID:?publisher run required}" "${GITHUB_RUN_ATTEMPT:?publisher attempt required}"
    job=$(gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/attempts/$GITHUB_RUN_ATTEMPT/jobs?per_page=100" | jq -er --arg name "Publish $IMAGE_REPOSITORY" '[.[].jobs[]?|select(.name==$name)] | if length==1 then .[0].id else error("ambiguous publisher job") end')
    [[ $job =~ ^[1-9][0-9]*$ ]]
    printf '%s' "$MONDAY_RELEASE_SIGNING_KEY" >"$work/key"
    unset MONDAY_RELEASE_SIGNING_KEY
    jq -n --arg source "$SOURCE_REVISION" --argjson software_run "$PRODUCER_RUN" --arg products "$SOFTWARE_PRODUCTS" --arg product "$PRODUCT" --arg image "$IMAGE" --argjson run "$GITHUB_RUN_ID" --argjson attempt "$GITHUB_RUN_ATTEMPT" --argjson job "$job" \
      '{source_sha:$source,software_run_id:$software_run,software_products:$products,product:$product,image:$image,publisher_run_id:$run,publisher_run_attempt:$attempt,publisher_job_id:$job}' >"$work/request.json"
    native=(cargo run --manifest-path "$root/rust_hft/research-core/platform/Cargo.toml" --locked --features publisher --bin research-release-publisher --)
    "${native[@]}" plan "$root" "$work/request.json" "$work/policy.json" >"${RUNNER_TEMP:?}/research-build-plan.json"
    "${native[@]}" publish "$root" "$work/request.json" "$work/policy.json" "$work/key" "$MONDAY_RELEASE_GATEWAY" "$work/token" >"$RUNNER_TEMP/research-build-artifacts.json"
    ;;
  import)
    : "${MONDAY_RELEASE_IMPORT_DATABASE_URL:?dedicated Build importer PG URL required}"
    jq .trust "$work/policy.json" >"$work/trust.json"
    jq '.tls // {}' "$work/policy.json" >"$work/tls.json"
    export MONDAY_RESEARCH_RELEASE_TLS_FILE="$work/tls.json"
    jq -e 'length>0 and all(.[]; [.build_sha256,.image_sha256,.publication_proof_sha256] | all(.[];test("^[0-9a-f]{64}$")))' "$RUNNER_TEMP/research-build-artifacts.json" >/dev/null
    # Selectors came from the native issuer; importer still verifies signature,
    # proof identity, actual source/program bytes and PG projection independently.
    while IFS=$'\t' read -r build oci proof; do
      MONDAY_RESEARCH_DATABASE_URL="$MONDAY_RELEASE_IMPORT_DATABASE_URL" cargo run --manifest-path "$root/rust_hft/research-core/platform/Cargo.toml" --locked --features publisher --bin research-release-publisher -- import "$build" "$oci" "$proof" "$work/trust.json" "$MONDAY_RELEASE_GATEWAY" "$work/token"
    done < <(jq -r '.[] | [.build_sha256,.image_sha256,.publication_proof_sha256] | @tsv' "$RUNNER_TEMP/research-build-artifacts.json")
    ;;
  *) exit 2 ;;
esac
