#!/usr/bin/env bash
# Control-plane bridge only: research code executes in the independently
# admitted private ACK executor. Public PR YAML never registers an ACK runner.
set -euo pipefail
profile=${1:?fixed ACK profile required}
source_sha=${2:?actual checkout SHA required}
destination=${3:-${RUNNER_TEMP:?}/ack-receipt}
case "$profile" in
  ci-research-preflight|ci-rust|ci-rust-fast-gates|security-clippy-research|research-image-binaries|research-image-smoke|prediction-research-format|prediction-research-heavy|research-release-binaries|research-release-publish|research-source-test) ;;
  *) echo 'Unknown private ACK execution profile' >&2; exit 2 ;;
esac
[[ $source_sha =~ ^[0-9a-f]{40}$ && ${GITHUB_RUN_ID:-} =~ ^[0-9]+$ && ${GITHUB_JOB:-} =~ ^[a-zA-Z0-9_-]+$ ]] || exit 2
[[ ${GITHUB_REPOSITORY:-} == proerror77/monday ]] || exit 2
if [[ ${GITHUB_EVENT_NAME:-} == pull_request ]]; then
  head_repository=$(jq -er '.pull_request.head.repo.full_name' "${GITHUB_EVENT_PATH:?}")
  [[ $head_repository == "$GITHUB_REPOSITORY" ]] || {
    echo 'Fork research jobs require independent source admission; no hosted compiler fallback.' >&2
    exit 1
  }
fi
v2=false
case "$profile" in
  ci-research-preflight) v2=true ;;
  ci-rust|security-clippy-research|research-image-binaries)
    # Migration is source-scoped, never a caller toggle or unsigned receipt hint.
    mkdir -p "$destination"
    selected_base=$(jq -r '.pull_request.base.sha // .before // empty' "$GITHUB_EVENT_PATH")
    selected_head=$(jq -r '.pull_request.head.sha // .after // empty' "$GITHUB_EVENT_PATH")
    [[ -n $selected_head ]] || selected_head=$source_sha
    GITHUB_OUTPUT="$destination/derived-scope.txt" bash .github/scripts/select-rust-ci-scope.sh --event "$GITHUB_EVENT_NAME" --base "$selected_base" --head "$selected_head"
    selected_collector=$(awk -F= '$1=="collector" {print $2}' "$destination/derived-scope.txt")
    [[ $selected_collector == true || $selected_collector == false ]] || exit 1
    [[ $selected_collector != true ]] || v2=true ;;
esac
attempt=${GITHUB_RUN_ATTEMPT:-0}
if [[ $v2 == true ]]; then [[ $attempt =~ ^[1-9][0-9]*$ ]] || exit 2; fi
mkdir -p "$destination"
jq -n --arg schema monday.ack_execution_request.v1 \
  --arg repository "$GITHUB_REPOSITORY" --arg run_id "$GITHUB_RUN_ID" \
  --arg job "$GITHUB_JOB" --arg source "$source_sha" --arg profile "$profile" \
  '{schema_version:$schema,public_repo:$repository,public_run_id:$run_id,public_job:$job,checkout_sha:$source,profile:$profile}' \
  > "$destination/request.json"
if [[ $v2 == true ]]; then
  jq --argjson attempt "$attempt" '. + {schema_version:"monday.ack_execution_request.v2",public_run_attempt:$attempt}' "$destination/request.json" >"$destination/request-v2.json"
  mv "$destination/request-v2.json" "$destination/request.json"
fi
cat "$destination/request.json"
printf '\nWaiting for an independently verified ACK terminal receipt. No research code runs in this job.\n'

repo_root=$(git rev-parse --show-toplevel)
public_key="$repo_root/.github/ack-ci/receipt-public-key.pub"
[[ -f $public_key ]] || exit 1
base="https://raw.githubusercontent.com/proerror77/monday/codex/ack-ci-receipts/$GITHUB_RUN_ID/$GITHUB_JOB/$source_sha"
if [[ $v2 == true ]]; then base="https://raw.githubusercontent.com/proerror77/monday/codex/ack-ci-receipts/$GITHUB_RUN_ID/$attempt/$GITHUB_JOB/$source_sha"; fi
schema=monday.ack_execution_receipt.v1
[[ $v2 == false ]] || schema=monday.ack_execution_receipt.v2
timeout=${ACK_RECEIPT_TIMEOUT_SECONDS:-18000}
[[ $timeout =~ ^[0-9]+$ ]] && ((timeout >= 60 && timeout <= 21600)) || exit 2
deadline=$(( $(date +%s) + timeout ))
while (( $(date +%s) < deadline )); do
  stamp=$(date +%s)
  if curl --connect-timeout 10 --max-time 20 --max-filesize 262144 -fsS \
       "$base/receipt.json?time=$stamp" -o "$destination/receipt.json" 2>/dev/null &&
     curl --connect-timeout 10 --max-time 20 --max-filesize 1024 -fsS \
       "$base/receipt.sig?time=$stamp" -o "$destination/receipt.sig" 2>/dev/null; then
    openssl pkeyutl -verify -pubin -inkey "$public_key" -rawin \
      -in "$destination/receipt.json" -sigfile "$destination/receipt.sig" >/dev/null || exit 1
    jq -e --arg source "$source_sha" --arg profile "$profile" --arg run "$GITHUB_RUN_ID" --arg job "$GITHUB_JOB" --arg schema "$schema" '
      .schema_version == $schema and
      .public_repo == "proerror77/monday" and .public_run_id == $run and .public_job == $job and
      .checkout_sha == $source and .profile == $profile and .execution_host == "ack" and
      (.private_run_id | type == "string" and test("^[0-9]+$")) and
      (.command_manifest_sha256 | type == "string" and test("^[0-9a-f]{64}$")) and
      (.terminal_result == "success" or .terminal_result == "failure")
    ' "$destination/receipt.json" >/dev/null || exit 1
    if [[ $v2 == true ]]; then
      source "$repo_root/.github/scripts/verify-ack-preflight.sh"
      jq -e --argjson attempt "$attempt" --arg event "$GITHUB_EVENT_NAME" '.public_run_attempt==$attempt and .event==$event and (.scope_sha256|test("^[0-9a-f]{64}$"))' "$destination/receipt.json" >/dev/null || exit 1
      if [[ $GITHUB_EVENT_NAME == pull_request ]]; then
        jq -e --slurpfile event "$GITHUB_EVENT_PATH" '.head_sha==$event[0].pull_request.head.sha and .base_sha==$event[0].pull_request.base.sha' "$destination/receipt.json" >/dev/null || exit 1
      fi
      finished=$(date -u -d "$(jq -er .finished_at "$destination/receipt.json")" +%s) || exit 1
      expires=$(date -u -d "$(jq -er .expires_at "$destination/receipt.json")" +%s) || exit 1
      (( finished <= $(date +%s) && $(date +%s) < expires && expires - finished <= 28800 )) || exit 1
      if [[ $profile == ci-research-preflight ]]; then
        jq -e '.phase_results=={preflight:"success",quick:"success"}' "$destination/receipt.json" >/dev/null || exit 1
      else
        # Public GETs only: no runner credential, dispatch or added permissions.
        jq -e '.preflight | (.public_run_id|type=="number" and .>0 and floor==.) and (.public_run_attempt|type=="number" and .>0 and floor==.) and (.public_job_id|type=="number" and .>0 and floor==.) and (.receipt_sha256|test("^[0-9a-f]{64}$"))' "$destination/receipt.json" >/dev/null || exit 1
        producer=$(jq -r .preflight.public_run_id "$destination/receipt.json")
        producer_attempt=$(jq -r .preflight.public_run_attempt "$destination/receipt.json")
        proof_base="https://raw.githubusercontent.com/proerror77/monday/codex/ack-ci-receipts/$producer/$producer_attempt/research_preflight/$source_sha"
        curl --connect-timeout 10 --max-time 20 --max-filesize 262144 -fsS "$proof_base/receipt.json" -o "$destination/preflight.json"
        curl --connect-timeout 10 --max-time 20 --max-filesize 1024 -fsS "$proof_base/receipt.sig" -o "$destination/preflight.sig"
        curl --connect-timeout 10 --max-time 20 --max-filesize 262144 -fsS "https://api.github.com/repos/proerror77/monday/actions/runs/$producer" -o "$destination/producer.json"
        curl --connect-timeout 10 --max-time 20 --max-filesize 262144 -fsS "https://api.github.com/repos/proerror77/monday/actions/runs/$producer/attempts/$producer_attempt/jobs?per_page=100" -o "$destination/producer-jobs.json"
        source_ref=$(jq -r .source_ref "$destination/receipt.json")
        if [[ $source_ref =~ ^refs/pull/([0-9]+)/merge$ ]]; then
          source_api="pulls/${BASH_REMATCH[1]}"
        elif [[ $source_ref == refs/heads/* ]]; then source_api="git/ref/${source_ref#refs/}"
        else exit 1; fi
        curl --connect-timeout 10 --max-time 20 --max-filesize 262144 -fsS "https://api.github.com/repos/proerror77/monday/$source_api" -o "$destination/current-source.json"
        ack_verify_preflight "$destination/preflight.json" "$destination/preflight.sig" "$public_key" "$destination/receipt.json" "$destination/producer.json" "$destination/producer-jobs.json" "$(date +%s)" "$destination/current-source.json" || exit 1
      fi
    fi
    cat "$destination/receipt.json"
    [[ $(jq -r .terminal_result "$destination/receipt.json") == success ]] || exit 1
    if jq -e '.software_bundle != null' "$destination/receipt.json" >/dev/null; then
      bundle_url=$(jq -er '.software_bundle.url | select(startswith("https://"))' "$destination/receipt.json")
      bundle_sha=$(jq -er '.software_bundle.sha256 | select(test("^[0-9a-f]{64}$"))' "$destination/receipt.json")
      bundle_bytes=$(jq -er '.software_bundle.bytes | select(type=="number" and .>0 and .<=536870912)' "$destination/receipt.json")
      curl --connect-timeout 10 --max-time 600 --max-filesize "$bundle_bytes" -fsS "$bundle_url" -o "$destination/software.tar.gz"
      printf '%s  %s\n' "$bundle_sha" "$destination/software.tar.gz" | sha256sum -c -
      # This is reviewed software, never market data or a model/ledger archive.
      # The bundle comes from the immutable private executor and is not executed
      # by the public control job.
      mkdir -p "$destination/software"
      tar --no-same-owner -xzf "$destination/software.tar.gz" -C "$destination/software"
    fi
    if [[ -n ${GITHUB_OUTPUT:-} ]]; then
      printf 'ack_receipt=%s\nprivate_run_id=%s\n' "$destination/receipt.json" \
        "$(jq -r .private_run_id "$destination/receipt.json")" >> "$GITHUB_OUTPUT"
    fi
    exit 0
  fi
  sleep 30
done
echo 'ACK terminal receipt deadline elapsed; the selected check cannot pass.' >&2
exit 1
